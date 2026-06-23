//! `hjkl --nvim-api` — msgpack-rpc server over stdin/stdout speaking the nvim
//! wire protocol. Phase 3 of issue #26.
//!
//! Wire format (msgpack-rpc spec):
//! - Request:      `[0, msgid: u32, method: String, params: Array]`
//! - Response:     `[1, msgid: u32, error: Value|Nil, result: Value|Nil]`
//! - Notification: `[2, method: String, params: Array]`
//!
//! Messages are framed as msgpack values with no additional length-prefix.
//! The server reads requests off stdin in a loop; responses are written to
//! stdout and flushed after each one.
//!
//! ## Buffer/window ext-type handles
//!
//! nvim-rs expects buffer handles as `Value::Ext(BUFFER_EXT, bytes)` and window
//! handles as `Value::Ext(WINDOW_EXT, bytes)`. The single supported buffer has
//! id=1, encoded as a msgpack positive fixint (0x01). Window id=1 likewise.
//!
//! ## Supported methods
//!
//! See the table in `docs/embed-rpc.md` — the "nvim-api mode" section.

use std::io::Write;
use std::path::PathBuf;

use anyhow::Result;
use hjkl_buffer::Buffer;
use hjkl_engine::{Host, VimMode};
use rmpv::Value;

use crate::host::TuiHost;

// ── ext-type tags (nvim wire protocol) ────────────────────────────────────────
const BUFFER_EXT: i8 = 0;
const WINDOW_EXT: i8 = 1;

/// Encode a u64 id as the minimal msgpack bytes for the ext payload.
/// nvim uses a fixint 1 (0x01) as the buffer/window id in practice.
fn encode_id(id: u64) -> Vec<u8> {
    let mut buf = Vec::new();
    rmpv::encode::write_value(&mut buf, &Value::from(id)).expect("encode ext id");
    buf
}

fn buf_handle(id: u64) -> Value {
    Value::Ext(BUFFER_EXT, encode_id(id))
}

/// Decode a `Value::Ext(BUFFER_EXT, bytes)` back to a buffer id.
/// If the param is missing, Nil, or decodes to 0, returns `None` (caller
/// substitutes the current buffer id).
fn param_buf(params: &[Value], idx: usize) -> std::result::Result<Option<u64>, String> {
    match params.get(idx) {
        None | Some(Value::Nil) => Ok(None),
        Some(Value::Ext(tag, bytes)) if *tag == BUFFER_EXT => {
            let mut cursor = std::io::Cursor::new(bytes.as_slice());
            match rmpv::decode::read_value(&mut cursor) {
                Ok(inner) => {
                    let id = inner.as_u64().unwrap_or(0);
                    Ok(if id == 0 { None } else { Some(id) })
                }
                Err(e) => Err(format!("invalid buffer handle encoding: {e}")),
            }
        }
        Some(Value::Integer(n)) => {
            // Some clients send a raw integer 0 meaning "current".
            let id = n.as_u64().unwrap_or(0);
            Ok(if id == 0 { None } else { Some(id) })
        }
        Some(other) => Err(format!(
            "params[{idx}] must be buffer handle, got {other:?}"
        )),
    }
}

fn win_handle() -> Value {
    Value::Ext(WINDOW_EXT, encode_id(1))
}

// ── I/O helpers ───────────────────────────────────────────────────────────────

fn write_response(stdout: &mut impl Write, msgid: u32, error: Value, result: Value) -> Result<()> {
    let msg = Value::Array(vec![
        Value::from(1u64), // response type
        Value::from(msgid as u64),
        error,
        result,
    ]);
    rmpv::encode::write_value(stdout, &msg)?;
    stdout.flush()?;
    Ok(())
}

fn ok(stdout: &mut impl Write, msgid: u32, result: Value) -> Result<()> {
    write_response(stdout, msgid, Value::Nil, result)
}

fn err(stdout: &mut impl Write, msgid: u32, msg: &str) -> Result<()> {
    write_response(
        stdout,
        msgid,
        Value::Array(vec![
            Value::from(0i64), // nvim error type: generic
            Value::from(msg),
        ]),
        Value::Nil,
    )
}

// ── App construction ──────────────────────────────────────────────────────────

fn build_app(first_file: Option<PathBuf>) -> anyhow::Result<crate::app::App> {
    let mut app = crate::app::App::new(first_file, false, None, None)?;
    {
        let vp = app.active_editor_mut().host_mut().viewport_mut();
        vp.width = 80;
        vp.height = 24;
    }
    Ok(app)
}

// ── settle helper ─────────────────────────────────────────────────────────────

fn settle(app: &mut crate::app::App) {
    app.reconcile_window_editors();
    if app.pending_recompute {
        app.pending_recompute = false;
        app.recompute_and_install();
    }
}

// ── Parameter extractors ──────────────────────────────────────────────────────

/// Get params as a slice.
fn as_array(params: &Value) -> std::result::Result<&[Value], String> {
    match params {
        Value::Array(v) => Ok(v.as_slice()),
        _ => Err("params must be an array".to_string()),
    }
}

fn param_i64(params: &[Value], idx: usize) -> std::result::Result<i64, String> {
    match params.get(idx) {
        Some(Value::Integer(n)) => n
            .as_i64()
            .ok_or_else(|| format!("params[{idx}] out of i64 range")),
        Some(other) => Err(format!("params[{idx}] must be integer, got {other:?}")),
        None => Err(format!("params[{idx}] missing")),
    }
}

fn param_bool(params: &[Value], idx: usize) -> std::result::Result<bool, String> {
    match params.get(idx) {
        Some(Value::Boolean(b)) => Ok(*b),
        Some(_) | None => Ok(false), // nvim-rs often sends Nil for the strict flag
    }
}

fn param_str(params: &[Value], idx: usize) -> std::result::Result<String, String> {
    match params.get(idx) {
        Some(Value::String(s)) => s
            .as_str()
            .map(|s| s.to_owned())
            .ok_or_else(|| format!("params[{idx}] not valid UTF-8")),
        Some(other) => Err(format!("params[{idx}] must be string, got {other:?}")),
        None => Err(format!("params[{idx}] missing")),
    }
}

fn param_string_array(params: &[Value], idx: usize) -> std::result::Result<Vec<String>, String> {
    match params.get(idx) {
        Some(Value::Array(arr)) => arr
            .iter()
            .enumerate()
            .map(|(i, v)| match v {
                Value::String(s) => s
                    .as_str()
                    .map(|s| s.to_owned())
                    .ok_or_else(|| format!("params[{idx}][{i}] not valid UTF-8")),
                other => Err(format!("params[{idx}][{i}] must be string, got {other:?}")),
            })
            .collect(),
        Some(other) => Err(format!("params[{idx}] must be array, got {other:?}")),
        None => Err(format!("params[{idx}] missing")),
    }
}

// ── nvim_get_mode helper ───────────────────────────────────────────────────────

fn mode_code(editor: &hjkl_engine::Editor<Buffer, TuiHost>) -> &'static str {
    match editor.vim_mode() {
        VimMode::Normal => "n",
        VimMode::Insert => "i",
        VimMode::Visual => "v",
        VimMode::VisualLine => "V",
        VimMode::VisualBlock => "\x16",
    }
}

// ── buffer line range helper ──────────────────────────────────────────────────

/// Resolve nvim-style [start, end) line indices (end=-1 means to the last
/// line) into a concrete Rust range over the buffer's lines. Both `start` and
/// `end` are 0-based. Returns an error string if out of bounds.
fn resolve_line_range(
    line_count: usize,
    start: i64,
    end: i64,
) -> std::result::Result<(usize, usize), String> {
    let n = line_count as i64;
    let s = if start < 0 {
        (n + start).max(0) as usize
    } else {
        start as usize
    };
    let e = if end < 0 {
        (n + end + 1).max(0) as usize
    } else {
        end as usize
    };
    let e = e.min(line_count);
    if s > e {
        return Err(format!(
            "line range out of order: start={start} end={end} (resolved {s}..{e})"
        ));
    }
    Ok((s, e))
}

// ── method dispatch ───────────────────────────────────────────────────────────

fn dispatch(
    app: &mut crate::app::App,
    should_quit: &mut bool,
    method: &str,
    params: &Value,
    stdout: &mut impl Write,
    msgid: u32,
) -> Result<()> {
    match method {
        // ── buffer/window handle accessors ────────────────────────────────────
        "nvim_get_current_buf" => ok(stdout, msgid, buf_handle(app.nvim_current_buffer_id())),

        "nvim_list_bufs" => {
            let handles: Vec<Value> = app.nvim_buffer_ids().into_iter().map(buf_handle).collect();
            ok(stdout, msgid, Value::Array(handles))
        }

        "nvim_set_current_buf" => {
            let p = match as_array(params) {
                Ok(p) => p,
                Err(e) => return err(stdout, msgid, &e),
            };
            let id = match param_buf(p, 0) {
                Ok(Some(id)) => id,
                Ok(None) => app.nvim_current_buffer_id(),
                Err(e) => return err(stdout, msgid, &e),
            };
            match app.nvim_slot_index_for_buffer(id) {
                Some(i) => {
                    app.switch_to(i);
                    app.sync_after_engine_mutation();
                    settle(app);
                    ok(stdout, msgid, Value::Nil)
                }
                None => err(stdout, msgid, "invalid buffer id"),
            }
        }

        "nvim_create_buf" => {
            // nvim_create_buf(listed: bool, scratch: bool) — both ignored for now.
            let new_id = app.nvim_create_buffer();
            ok(stdout, msgid, buf_handle(new_id))
        }

        "nvim_buf_get_name" => {
            let p = match as_array(params) {
                Ok(p) => p,
                Err(e) => return err(stdout, msgid, &e),
            };
            let id = match param_buf(p, 0) {
                Ok(Some(id)) => id,
                Ok(None) => app.nvim_current_buffer_id(),
                Err(e) => return err(stdout, msgid, &e),
            };
            let name = app.nvim_buffer_name(id).unwrap_or_default();
            ok(stdout, msgid, Value::from(name.as_str()))
        }

        "nvim_buf_set_name" => {
            let p = match as_array(params) {
                Ok(p) => p,
                Err(e) => return err(stdout, msgid, &e),
            };
            let id = match param_buf(p, 0) {
                Ok(Some(id)) => id,
                Ok(None) => app.nvim_current_buffer_id(),
                Err(e) => return err(stdout, msgid, &e),
            };
            let name = match param_str(p, 1) {
                Ok(s) => s,
                Err(e) => return err(stdout, msgid, &e),
            };
            app.nvim_set_buffer_name(id, &name);
            ok(stdout, msgid, Value::Nil)
        }

        "nvim_get_current_win" => ok(stdout, msgid, win_handle()),

        // ── buffer line mutations ─────────────────────────────────────────────
        "nvim_buf_set_lines" => {
            // nvim_buf_set_lines(buf, start, end, strict, lines)
            // params[0]=buf handle, [1]=start, [2]=end, [3]=strict, [4]=replacement lines
            let p = match as_array(params) {
                Ok(p) => p,
                Err(e) => return err(stdout, msgid, &e),
            };
            let buf_id = match param_buf(p, 0) {
                Ok(Some(id)) => id,
                Ok(None) => app.nvim_current_buffer_id(),
                Err(e) => return err(stdout, msgid, &e),
            };
            // param index: buf=0, start=1, end=2, strict=3, lines=4
            let start = match param_i64(p, 1) {
                Ok(v) => v,
                Err(e) => return err(stdout, msgid, &e),
            };
            let end = match param_i64(p, 2) {
                Ok(v) => v,
                Err(e) => return err(stdout, msgid, &e),
            };
            let _strict = param_bool(p, 3).unwrap_or(false);
            let new_lines = match param_string_array(p, 4) {
                Ok(v) => v,
                Err(e) => return err(stdout, msgid, &e),
            };

            let current_id = app.nvim_current_buffer_id();
            if buf_id == current_id {
                // Fast path: operate on the active editor (oracle-parity path).
                let rope = app.active_editor().buffer().rope();
                let line_count = rope.len_lines();
                let (s, e) = match resolve_line_range(line_count, start, end) {
                    Ok(r) => r,
                    Err(msg) => return err(stdout, msgid, &msg),
                };

                // Build new full content: prefix[0..s] + new_lines + suffix[e..]
                let mut result: Vec<String> = Vec::new();
                for i in 0..s {
                    result.push(hjkl_buffer::rope_line_str(&rope, i));
                }
                result.extend(new_lines);
                for i in e..line_count {
                    result.push(hjkl_buffer::rope_line_str(&rope, i));
                }

                // Join WITHOUT a trailing newline. BufferEdit::replace_all uses
                // split('\n') internally, so "hello\n" → ["hello", ""] (two rows)
                // whereas "hello" → ["hello"] (one row, matching nvim semantics).
                let content = result.join("\n");
                app.active_editor_mut().set_content(&content);
                // Apply modeline overrides so oracle cases that embed a `vim:`
                // marker see the same options that a real file-open would apply.
                {
                    let mut opts = app.active_editor().current_options();
                    if opts.modeline {
                        let scan_depth = opts.modelines as usize;
                        hjkl_app::modeline::overlay_modeline_for_content(
                            &mut opts, &content, scan_depth,
                        );
                        app.active_editor_mut().apply_options(&opts);
                    }
                }
            } else {
                // Non-current buffer: operate on the slot's own editor.
                let rope = match app.nvim_slot_editor(buf_id) {
                    Some(ed) => ed.buffer().rope(),
                    None => return err(stdout, msgid, "invalid buffer id"),
                };
                let line_count = rope.len_lines();
                let (s, e) = match resolve_line_range(line_count, start, end) {
                    Ok(r) => r,
                    Err(msg) => return err(stdout, msgid, &msg),
                };

                let mut result: Vec<String> = Vec::new();
                for i in 0..s {
                    result.push(hjkl_buffer::rope_line_str(&rope, i));
                }
                result.extend(new_lines);
                for i in e..line_count {
                    result.push(hjkl_buffer::rope_line_str(&rope, i));
                }
                let content = result.join("\n");
                match app.nvim_slot_editor_mut(buf_id) {
                    Some(ed) => ed.set_content(&content),
                    None => return err(stdout, msgid, "invalid buffer id"),
                }
            }
            settle(app);
            ok(stdout, msgid, Value::Nil)
        }

        "nvim_buf_get_lines" => {
            // nvim_buf_get_lines(buf, start, end, strict)
            let p = match as_array(params) {
                Ok(p) => p,
                Err(e) => return err(stdout, msgid, &e),
            };
            let buf_id = match param_buf(p, 0) {
                Ok(Some(id)) => id,
                Ok(None) => app.nvim_current_buffer_id(),
                Err(e) => return err(stdout, msgid, &e),
            };
            let start = match param_i64(p, 1) {
                Ok(v) => v,
                Err(e) => return err(stdout, msgid, &e),
            };
            let end = match param_i64(p, 2) {
                Ok(v) => v,
                Err(e) => return err(stdout, msgid, &e),
            };
            let _strict = param_bool(p, 3).unwrap_or(false);

            let current_id = app.nvim_current_buffer_id();
            let rope = if buf_id == current_id {
                app.active_editor().buffer().rope()
            } else {
                match app.nvim_slot_editor(buf_id) {
                    Some(ed) => ed.buffer().rope(),
                    None => return err(stdout, msgid, "invalid buffer id"),
                }
            };
            let line_count = rope.len_lines();
            let (s, e) = match resolve_line_range(line_count, start, end) {
                Ok(r) => r,
                Err(msg) => return err(stdout, msgid, &msg),
            };
            let result: Vec<Value> = (s..e)
                .map(|i| Value::from(hjkl_buffer::rope_line_str(&rope, i)))
                .collect();
            ok(stdout, msgid, Value::Array(result))
        }

        // ── cursor ────────────────────────────────────────────────────────────
        "nvim_win_set_cursor" => {
            // nvim_win_set_cursor(win, [row, col])
            // row is 1-based; col is byte-col (we treat as char-col for ASCII).
            let p = match as_array(params) {
                Ok(p) => p,
                Err(e) => return err(stdout, msgid, &e),
            };
            // params[0]=win handle (ignored), params[1]=[row, col]
            let pair = match p.get(1) {
                Some(Value::Array(arr)) => arr.as_slice(),
                Some(other) => {
                    return err(
                        stdout,
                        msgid,
                        &format!("params[1] must be [row, col] array, got {other:?}"),
                    );
                }
                None => return err(stdout, msgid, "params[1] missing"),
            };
            let row_1based = match pair.first() {
                Some(Value::Integer(n)) => n.as_i64().unwrap_or(1),
                _ => return err(stdout, msgid, "cursor row must be integer"),
            };
            let col = match pair.get(1) {
                Some(Value::Integer(n)) => n.as_i64().unwrap_or(0),
                _ => return err(stdout, msgid, "cursor col must be integer"),
            };
            // Convert 1-based nvim row to 0-based engine row.
            let row = (row_1based - 1).max(0) as usize;
            // For byte-col → char-col: walk the line's chars (ASCII = identity).
            let char_col = {
                let rope = app.active_editor().buffer().rope();
                if row < rope.len_lines() {
                    let line = hjkl_buffer::rope_line_str(&rope, row);
                    let byte_offset = (col as usize).min(line.len());
                    line[..byte_offset].chars().count()
                } else {
                    0
                }
            };
            app.active_editor_mut().jump_cursor(row, char_col);
            ok(stdout, msgid, Value::Nil)
        }

        "nvim_win_get_cursor" => {
            // Returns [row (1-based), col (byte-col)].
            let (row, char_col) = app.active_editor().cursor();
            // Convert char-col to byte-col.
            let byte_col = {
                let rope = app.active_editor().buffer().rope();
                if row < rope.len_lines() {
                    let line = hjkl_buffer::rope_line_str(&rope, row);
                    line.chars()
                        .take(char_col)
                        .map(|c| c.len_utf8())
                        .sum::<usize>()
                } else {
                    char_col
                }
            };
            let result = Value::Array(vec![
                Value::from((row + 1) as i64), // 1-based
                Value::from(byte_col as i64),
            ]);
            ok(stdout, msgid, result)
        }

        // ── input ─────────────────────────────────────────────────────────────
        "nvim_input" => {
            let p = match as_array(params) {
                Ok(p) => p,
                Err(e) => return err(stdout, msgid, &e),
            };
            let keys = match param_str(p, 0) {
                Ok(s) => s,
                Err(e) => return err(stdout, msgid, &e),
            };
            let len = keys.len() as i64;
            let inputs = hjkl_engine::decode_macro(&keys);
            for input in inputs {
                hjkl_vim::dispatch_input(app.active_editor_mut(), input);
            }
            settle(app);
            ok(stdout, msgid, Value::from(len))
        }

        // ── ex command ────────────────────────────────────────────────────────
        "nvim_command" => {
            let p = match as_array(params) {
                Ok(p) => p,
                Err(e) => return err(stdout, msgid, &e),
            };
            let cmd_raw = match param_str(p, 0) {
                Ok(s) => s,
                Err(e) => return err(stdout, msgid, &e),
            };
            let cmd = cmd_raw.strip_prefix(':').unwrap_or(&cmd_raw).to_string();
            app.dispatch_ex(&cmd);
            settle(app);
            if app.exit_requested {
                *should_quit = true;
            }
            ok(stdout, msgid, Value::Nil)
        }

        // ── mode ──────────────────────────────────────────────────────────────
        "nvim_get_mode" => {
            let code = mode_code(app.active_editor());
            // Returns Map: {mode: str, blocking: false}
            let map = Value::Map(vec![
                (Value::from("mode"), Value::from(code)),
                (Value::from("blocking"), Value::Boolean(false)),
            ]);
            ok(stdout, msgid, map)
        }

        // ── registers via nvim_call_function("getreg", [name]) ────────────────
        "nvim_call_function" => {
            let p = match as_array(params) {
                Ok(p) => p,
                Err(e) => return err(stdout, msgid, &e),
            };
            let fn_name = match param_str(p, 0) {
                Ok(s) => s,
                Err(e) => return err(stdout, msgid, &e),
            };
            if fn_name != "getreg" {
                return err(
                    stdout,
                    msgid,
                    &format!("nvim_call_function: unsupported function: {fn_name}"),
                );
            }
            // params[1] is the argument array for the function.
            let fn_args = match p.get(1) {
                Some(Value::Array(arr)) => arr.as_slice(),
                _ => return err(stdout, msgid, "nvim_call_function: params[1] must be array"),
            };
            let reg_name = match fn_args.first() {
                Some(Value::String(s)) => s.as_str().unwrap_or("\"").to_owned(),
                _ => "\"".to_owned(),
            };
            let reg_char = reg_name.chars().next().unwrap_or('"');
            let text = match app.active_editor().registers().read(reg_char) {
                Some(slot) => slot.text.clone(),
                None => String::new(),
            };
            ok(stdout, msgid, Value::from(text.as_str()))
        }

        // ── synchronisation barrier ───────────────────────────────────────────
        // The oracle calls `nvim.command("echo 1")` as a barrier. Handle it.
        _ => err(stdout, msgid, &format!("method not implemented: {method}")),
    }
}

// ── public entry point ────────────────────────────────────────────────────────

/// Run in nvim-api mode: msgpack-rpc server over stdin/stdout.
///
/// `files` — files to open. Only the first is loaded (single-buffer for now).
pub fn run(files: Vec<PathBuf>) -> Result<i32> {
    let mut app = build_app(files.into_iter().next())?;
    let mut should_quit = false;

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut stdin_lock = stdin.lock();
    let mut stdout_lock = stdout.lock();

    // We need a buffered reader to pull bytes as they arrive.
    let mut reader = std::io::BufReader::new(&mut stdin_lock);

    loop {
        // Read one msgpack value. Returns Err on EOF or protocol error.
        let msg = match rmpv::decode::read_value(&mut reader) {
            Ok(v) => v,
            Err(e) => {
                use rmpv::decode::Error;
                match e {
                    Error::InvalidMarkerRead(io) | Error::InvalidDataRead(io)
                        if io.kind() == std::io::ErrorKind::UnexpectedEof =>
                    {
                        // EOF — clean exit.
                        break;
                    }
                    _ => {
                        // Protocol error; we can't know the msgid, skip.
                        eprintln!("hjkl --nvim-api: decode error: {e}");
                        continue;
                    }
                }
            }
        };

        // Expect Array [type, msgid, method, params]
        let arr = match &msg {
            Value::Array(a) => a.as_slice(),
            _ => {
                eprintln!("hjkl --nvim-api: expected array, got {msg:?}");
                continue;
            }
        };

        // type: 0=request, 2=notification
        let msg_type = match arr.first() {
            Some(Value::Integer(n)) => n.as_u64().unwrap_or(99),
            _ => {
                eprintln!("hjkl --nvim-api: bad message type");
                continue;
            }
        };

        match msg_type {
            0 => {
                // Request: [0, msgid, method, params]
                let msgid = match arr.get(1) {
                    Some(Value::Integer(n)) => n.as_u64().unwrap_or(0) as u32,
                    _ => {
                        eprintln!("hjkl --nvim-api: missing msgid");
                        continue;
                    }
                };
                let method = match arr.get(2) {
                    Some(Value::String(s)) => s.as_str().unwrap_or("").to_owned(),
                    _ => {
                        let _ = err(&mut stdout_lock, msgid, "missing method");
                        continue;
                    }
                };
                let params = arr.get(3).cloned().unwrap_or(Value::Array(vec![]));
                dispatch(
                    &mut app,
                    &mut should_quit,
                    &method,
                    &params,
                    &mut stdout_lock,
                    msgid,
                )?;
                if should_quit {
                    break;
                }
            }
            2 => {
                // Notification: [2, method, params] — dispatch, no response.
                let method = match arr.get(1) {
                    Some(Value::String(s)) => s.as_str().unwrap_or("").to_owned(),
                    _ => continue,
                };
                let params = arr.get(2).cloned().unwrap_or(Value::Array(vec![]));
                // Use a dummy msgid=0; response is suppressed.
                let mut dev_null = std::io::sink();
                dispatch(
                    &mut app,
                    &mut should_quit,
                    &method,
                    &params,
                    &mut dev_null,
                    0,
                )?;
                if should_quit {
                    break;
                }
            }
            _ => {
                eprintln!("hjkl --nvim-api: unexpected message type {msg_type}");
            }
        }
    }

    Ok(0)
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rmpv::Value;

    /// Encode a buffer handle `Value::Ext(BUFFER_EXT, encode_id(id))` suitable
    /// for passing as a param to dispatch.
    #[allow(dead_code)]
    fn make_buf_param(id: u64) -> Value {
        buf_handle(id)
    }

    /// Decode the msgpack-rpc response written to `out` and return
    /// `[type, msgid, error, result]` as a `Vec<Value>`.
    fn decode_response(out: &[u8]) -> Vec<Value> {
        let mut cursor = std::io::Cursor::new(out);
        match rmpv::decode::read_value(&mut cursor).expect("decode_response") {
            Value::Array(arr) => arr,
            other => panic!("expected array response, got {other:?}"),
        }
    }

    fn call(app: &mut crate::app::App, method: &str, params: Vec<Value>) -> Vec<Value> {
        let mut out: Vec<u8> = Vec::new();
        let mut quit = false;
        dispatch(app, &mut quit, method, &Value::Array(params), &mut out, 1)
            .expect("dispatch error");
        decode_response(&out)
    }

    /// Assert a response is a success (error slot is Nil) and return the result.
    fn assert_ok(resp: Vec<Value>) -> Value {
        assert_eq!(resp[2], Value::Nil, "expected no error, got {:?}", resp[2]);
        resp[3].clone()
    }

    #[test]
    fn test_nvim_create_buf_returns_new_handle() {
        let mut app = build_app(None).unwrap();
        let resp = call(
            &mut app,
            "nvim_create_buf",
            vec![Value::Boolean(true), Value::Boolean(false)],
        );
        let result = assert_ok(resp);
        // Must be an Ext with BUFFER_EXT tag and a non-zero id.
        match &result {
            Value::Ext(tag, bytes) => {
                assert_eq!(*tag, BUFFER_EXT);
                let mut cur = std::io::Cursor::new(bytes.as_slice());
                let inner = rmpv::decode::read_value(&mut cur).unwrap();
                let id = inner.as_u64().expect("id should be integer");
                assert!(id > 0, "new buffer id should be > 0");
            }
            other => panic!("expected Ext buffer handle, got {other:?}"),
        }
    }

    #[test]
    fn test_nvim_list_bufs_grows_after_create() {
        let mut app = build_app(None).unwrap();

        let before = {
            let resp = call(&mut app, "nvim_list_bufs", vec![]);
            let result = assert_ok(resp);
            match result {
                Value::Array(v) => v.len(),
                other => panic!("expected array, got {other:?}"),
            }
        };

        // Create a new buffer.
        call(
            &mut app,
            "nvim_create_buf",
            vec![Value::Boolean(true), Value::Boolean(false)],
        );

        let after = {
            let resp = call(&mut app, "nvim_list_bufs", vec![]);
            let result = assert_ok(resp);
            match result {
                Value::Array(v) => v.len(),
                other => panic!("expected array, got {other:?}"),
            }
        };

        assert_eq!(after, before + 1, "list_bufs should grow by 1 after create");
    }

    #[test]
    fn test_nvim_set_current_buf_switches() {
        let mut app = build_app(None).unwrap();

        // Remember the initial current buffer id.
        let initial_handle = {
            let resp = call(&mut app, "nvim_get_current_buf", vec![]);
            assert_ok(resp)
        };

        // Create a new buffer.
        let new_handle = {
            let resp = call(
                &mut app,
                "nvim_create_buf",
                vec![Value::Boolean(true), Value::Boolean(false)],
            );
            assert_ok(resp)
        };

        // Switch to the new buffer.
        {
            let resp = call(&mut app, "nvim_set_current_buf", vec![new_handle.clone()]);
            assert_ok(resp);
        }

        // Current buf should now equal the new handle.
        let current_handle = {
            let resp = call(&mut app, "nvim_get_current_buf", vec![]);
            assert_ok(resp)
        };
        assert_eq!(
            current_handle, new_handle,
            "current buf should be the newly switched-to buffer"
        );
        assert_ne!(
            current_handle, initial_handle,
            "current buf should differ from initial"
        );
    }

    #[test]
    fn test_nvim_buf_set_name_get_name_roundtrip() {
        let mut app = build_app(None).unwrap();

        // Get the current buffer handle.
        let cur_handle = {
            let resp = call(&mut app, "nvim_get_current_buf", vec![]);
            assert_ok(resp)
        };

        // Set name.
        {
            let resp = call(
                &mut app,
                "nvim_buf_set_name",
                vec![
                    cur_handle.clone(),
                    Value::from("/tmp/test_hjkl_nvim_api_roundtrip.txt"),
                ],
            );
            assert_ok(resp);
        }

        // Get name back — should contain the path we set.
        let name = {
            let resp = call(&mut app, "nvim_buf_get_name", vec![cur_handle]);
            match assert_ok(resp) {
                Value::String(s) => s.as_str().unwrap_or("").to_owned(),
                other => panic!("expected string, got {other:?}"),
            }
        };

        // The name should contain the file part we set (canonical may differ on prefix).
        assert!(
            name.contains("test_hjkl_nvim_api_roundtrip.txt"),
            "expected name to contain 'test_hjkl_nvim_api_roundtrip.txt', got {name:?}"
        );
    }

    #[test]
    fn test_nvim_buf_get_lines_non_current_buffer() {
        let mut app = build_app(None).unwrap();

        // Create a second buffer and get its handle.
        let new_handle = {
            let resp = call(
                &mut app,
                "nvim_create_buf",
                vec![Value::Boolean(true), Value::Boolean(false)],
            );
            assert_ok(resp)
        };

        // Write lines into the new (non-current) buffer.
        {
            let resp = call(
                &mut app,
                "nvim_buf_set_lines",
                vec![
                    new_handle.clone(),
                    Value::from(0i64),
                    Value::from(-1i64),
                    Value::Boolean(false),
                    Value::Array(vec![
                        Value::from("hello from other buf"),
                        Value::from("second line"),
                    ]),
                ],
            );
            assert_ok(resp);
        }

        // Read them back — the current buffer should NOT be switched.
        let current_after = {
            let resp = call(&mut app, "nvim_get_current_buf", vec![]);
            assert_ok(resp)
        };
        // Current buf should still be the original (we never called set_current_buf).
        let initial_resp = call(&mut app, "nvim_list_bufs", vec![]);
        let bufs = match assert_ok(initial_resp) {
            Value::Array(v) => v,
            other => panic!("expected array, got {other:?}"),
        };
        assert_eq!(
            current_after, bufs[0],
            "current buf should still be the first (original) buffer"
        );

        // Get lines from the non-current buffer.
        let lines = {
            let resp = call(
                &mut app,
                "nvim_buf_get_lines",
                vec![
                    new_handle.clone(),
                    Value::from(0i64),
                    Value::from(-1i64),
                    Value::Boolean(false),
                ],
            );
            match assert_ok(resp) {
                Value::Array(v) => v,
                other => panic!("expected array, got {other:?}"),
            }
        };

        assert_eq!(lines.len(), 2, "should have 2 lines in non-current buffer");
        assert_eq!(lines[0], Value::from("hello from other buf"));
        assert_eq!(lines[1], Value::from("second line"));
    }
}
