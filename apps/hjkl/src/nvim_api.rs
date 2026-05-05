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
use hjkl_editor::runtime::ex::{self, ExEffect};
use hjkl_engine::{BufferEdit, DefaultHost, Editor, Options, VimMode};
use rmpv::Value;

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

fn buf_handle() -> Value {
    Value::Ext(BUFFER_EXT, encode_id(1))
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

// ── Editor construction ───────────────────────────────────────────────────────

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
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(anyhow::anyhow!("hjkl: {}: {e}", path.display())),
        }
    }
    let host = DefaultHost::new();
    let editor = Editor::new(buffer, host, Options::default());
    Ok((editor, maybe_path.cloned()))
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

fn mode_code(editor: &Editor<Buffer, DefaultHost>) -> &'static str {
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
    lines: &[String],
    start: i64,
    end: i64,
) -> std::result::Result<(usize, usize), String> {
    let n = lines.len() as i64;
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
    let e = e.min(lines.len());
    if s > e {
        return Err(format!(
            "line range out of order: start={start} end={end} (resolved {s}..{e})"
        ));
    }
    Ok((s, e))
}

// ── method dispatch ───────────────────────────────────────────────────────────

fn dispatch(
    editor: &mut Editor<Buffer, DefaultHost>,
    current_filename: &mut Option<PathBuf>,
    should_quit: &mut bool,
    method: &str,
    params: &Value,
    stdout: &mut impl Write,
    msgid: u32,
) -> Result<()> {
    match method {
        // ── buffer/window handle accessors ────────────────────────────────────
        "nvim_get_current_buf" => ok(stdout, msgid, buf_handle()),

        "nvim_get_current_win" => ok(stdout, msgid, win_handle()),

        // ── buffer line mutations ─────────────────────────────────────────────
        "nvim_buf_set_lines" => {
            // nvim_buf_set_lines(buf, start, end, strict, lines)
            // params[0]=buf handle (ignored, single buffer), [1]=start, [2]=end,
            // [3]=strict, [4]=replacement lines
            let p = match as_array(params) {
                Ok(p) => p,
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

            let current_lines = editor.buffer().lines().to_vec();
            let (s, e) = match resolve_line_range(&current_lines, start, end) {
                Ok(r) => r,
                Err(msg) => return err(stdout, msgid, &msg),
            };

            // Build new full content: prefix[0..s] + new_lines + suffix[e..]
            let mut result: Vec<String> = Vec::new();
            result.extend_from_slice(&current_lines[..s]);
            result.extend(new_lines);
            result.extend_from_slice(&current_lines[e..]);

            // Join WITHOUT a trailing newline. BufferEdit::replace_all uses
            // split('\n') internally, so "hello\n" → ["hello", ""] (two rows)
            // whereas "hello" → ["hello"] (one row, matching nvim semantics).
            let content = result.join("\n");
            editor.set_content(&content);
            ok(stdout, msgid, Value::Nil)
        }

        "nvim_buf_get_lines" => {
            // nvim_buf_get_lines(buf, start, end, strict)
            let p = match as_array(params) {
                Ok(p) => p,
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

            let lines = editor.buffer().lines();
            let (s, e) = match resolve_line_range(lines, start, end) {
                Ok(r) => r,
                Err(msg) => return err(stdout, msgid, &msg),
            };
            let result: Vec<Value> = lines[s..e]
                .iter()
                .map(|l| Value::from(l.as_str()))
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
                let lines = editor.buffer().lines();
                if let Some(line) = lines.get(row) {
                    // Count chars up to the byte offset.
                    let byte_offset = (col as usize).min(line.len());
                    line[..byte_offset].chars().count()
                } else {
                    0
                }
            };
            editor.jump_cursor(row, char_col);
            ok(stdout, msgid, Value::Nil)
        }

        "nvim_win_get_cursor" => {
            // Returns [row (1-based), col (byte-col)].
            let (row, char_col) = editor.cursor();
            // Convert char-col to byte-col.
            let byte_col = {
                let lines = editor.buffer().lines();
                if let Some(line) = lines.get(row) {
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
                hjkl_engine::step(editor, input);
            }
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
            let effect = ex::run(editor, &cmd);
            match effect {
                ExEffect::None
                | ExEffect::Ok
                | ExEffect::Info(_)
                | ExEffect::Substituted { .. } => ok(stdout, msgid, Value::Nil),
                ExEffect::Error(msg) | ExEffect::Unknown(msg) => err(stdout, msgid, &msg),
                ExEffect::Save => {
                    if let Err(e) = write_buffer(editor, current_filename) {
                        err(stdout, msgid, &e)
                    } else {
                        ok(stdout, msgid, Value::Nil)
                    }
                }
                ExEffect::SaveAs(path_str) => {
                    let new_path = PathBuf::from(&path_str);
                    if let Err(e) = write_buffer(editor, &Some(new_path.clone())) {
                        err(stdout, msgid, &e)
                    } else {
                        *current_filename = Some(new_path);
                        ok(stdout, msgid, Value::Nil)
                    }
                }
                ExEffect::Quit { save, force: _ } => {
                    if save && let Err(e) = write_buffer(editor, current_filename) {
                        return err(stdout, msgid, &e);
                    }
                    *should_quit = true;
                    ok(stdout, msgid, Value::Nil)
                }
            }
        }

        // ── mode ──────────────────────────────────────────────────────────────
        "nvim_get_mode" => {
            let code = mode_code(editor);
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
            let text = match editor.registers().read(reg_char) {
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

// ── buffer write helper ───────────────────────────────────────────────────────

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

// ── public entry point ────────────────────────────────────────────────────────

/// Run in nvim-api mode: msgpack-rpc server over stdin/stdout.
///
/// `files` — files to open. Only the first is loaded (single-buffer for now).
pub fn run(files: Vec<PathBuf>) -> Result<i32> {
    let first_file = files.into_iter().next();
    let (mut editor, mut current_filename) = build_editor(first_file.as_ref())?;
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
                    &mut editor,
                    &mut current_filename,
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
                    &mut editor,
                    &mut current_filename,
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
