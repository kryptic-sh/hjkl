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
//! ## View/window ext-type handles
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
use hjkl_buffer::View;
use hjkl_engine::{Host, VimMode};
use hjkl_quickfix::{QfEntry, QfKind};
use hjkl_vim::VimEditorExt;
use rmpv::Value;

use crate::host::TuiHost;

// ── ext-type tags (nvim wire protocol) ────────────────────────────────────────
const BUFFER_EXT: i8 = 0;
const WINDOW_EXT: i8 = 1;
const TABPAGE_EXT: i8 = 2;

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

fn win_handle(id: u64) -> Value {
    Value::Ext(WINDOW_EXT, encode_id(id))
}

/// Decode a `Value::Ext(WINDOW_EXT, bytes)` back to a window id.
/// Missing or Nil => returns `None` (caller substitutes the current window).
/// Unlike buffers, window id=0 is a valid real window (first window), so we
/// do NOT remap 0 to None.
fn param_win(params: &[Value], idx: usize) -> std::result::Result<Option<u64>, String> {
    match params.get(idx) {
        None | Some(Value::Nil) => Ok(None),
        Some(Value::Ext(tag, bytes)) if *tag == WINDOW_EXT => {
            let mut cursor = std::io::Cursor::new(bytes.as_slice());
            match rmpv::decode::read_value(&mut cursor) {
                Ok(inner) => Ok(Some(inner.as_u64().unwrap_or(0))),
                Err(e) => Err(format!("invalid window handle encoding: {e}")),
            }
        }
        Some(Value::Integer(n)) => {
            // Raw integer window id (some clients send these).
            Ok(Some(n.as_u64().unwrap_or(0)))
        }
        Some(other) => Err(format!(
            "params[{idx}] must be window handle, got {other:?}"
        )),
    }
}

fn tab_handle(id: u64) -> Value {
    Value::Ext(TABPAGE_EXT, encode_id(id))
}

/// Decode a `Value::Ext(TABPAGE_EXT, bytes)` back to a tabpage index.
/// Missing or Nil => returns `None` (caller substitutes `app.active_tab`).
fn param_tabpage(params: &[Value], idx: usize) -> std::result::Result<Option<u64>, String> {
    match params.get(idx) {
        None | Some(Value::Nil) => Ok(None),
        Some(Value::Ext(tag, bytes)) if *tag == TABPAGE_EXT => {
            let mut cursor = std::io::Cursor::new(bytes.as_slice());
            match rmpv::decode::read_value(&mut cursor) {
                Ok(inner) => Ok(Some(inner.as_u64().unwrap_or(0))),
                Err(e) => Err(format!("invalid tabpage handle encoding: {e}")),
            }
        }
        Some(Value::Integer(n)) => Ok(Some(n.as_u64().unwrap_or(0))),
        Some(other) => Err(format!(
            "params[{idx}] must be tabpage handle, got {other:?}"
        )),
    }
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
    use crate::app::STATUS_LINE_HEIGHT;
    // View-pane height = total terminal height minus the 1-row status line.
    const HEADLESS_W: u16 = 80;
    const HEADLESS_TERMINAL_H: u16 = 24;
    let buf_h = HEADLESS_TERMINAL_H.saturating_sub(STATUS_LINE_HEIGHT);

    let mut app = crate::app::App::new(first_file, false, None, None)?;
    {
        // Set the SLOT editor so make_view_editor() (called on every split)
        // copies the correct buffer-pane height to newly created window editors.
        {
            let vp = app.active_slot_mut().editor.host_mut().viewport_mut();
            vp.width = HEADLESS_W;
            vp.height = buf_h;
        }
        // Also propagate to the initial window editor (created by App::new's
        // reconcile_window_editors() call before we set the slot above).
        {
            let vp = app.active_editor_mut().host_mut().viewport_mut();
            vp.width = HEADLESS_W;
            vp.height = buf_h;
        }
    }
    Ok(app)
}

// ── headless window-geometry helpers ──────────────────────────────────────────

use hjkl_layout::{LayoutRect, SplitDir};

/// Return the headless buffer-pane area that the layout tree is divided into.
///
/// `build_app` stores the BUFFER-PANE height (total terminal height minus the
/// 1-row status line) directly in the slot editor and the initial window
/// editor. Subsequent splits inherit that height via `make_view_editor`.
/// Therefore `vp.height` already IS the buffer-pane height — no further
/// subtraction is needed here.
///
/// A fresh 80×24 headless app → `vp.height = 23` → single window height = 23,
/// matching neovim's reported value.
fn win_area(app: &crate::app::App) -> LayoutRect {
    let vp = app.active_editor().host().viewport();
    let w = vp.width.max(1);
    let h = vp.height.max(1);
    LayoutRect::new(0, 0, w, h)
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

fn mode_code(editor: &hjkl_engine::Editor<View, TuiHost>) -> &'static str {
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
///
/// Uses saturating arithmetic — extreme negative values from a hostile client
/// (e.g. `i64::MIN`) must clamp instead of overflowing.
fn resolve_line_range(
    line_count: usize,
    start: i64,
    end: i64,
) -> std::result::Result<(usize, usize), String> {
    let n = line_count as i64;
    let s = if start < 0 {
        n.saturating_add(start).max(0) as usize
    } else {
        start as usize
    };
    let e = if end < 0 {
        n.saturating_add(end).saturating_add(1).max(0) as usize
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

/// Convert an nvim byte-col into a char-col for `line`.
///
/// Clamps to the line's byte length, then snaps DOWN to the nearest UTF-8
/// char boundary — a client-supplied col landing mid-character must not
/// panic the slice below (RPC input is untrusted).
fn byte_col_to_char_col(line: &str, col: i64) -> usize {
    let mut byte_offset = (col as usize).min(line.len());
    while byte_offset > 0 && !line.is_char_boundary(byte_offset) {
        byte_offset -= 1;
    }
    line[..byte_offset].chars().count()
}

// ── nvim_call_function helpers ────────────────────────────────────────────────

/// Convert a `QfEntry` into the `Value::Map` dict that `getqflist` /
/// `getloclist` returns. Shape:
/// `{bufnr, lnum, col, text, valid}` (matches nvim wire semantics).
fn qf_entry_to_value(app: &crate::app::App, entry: &QfEntry) -> Value {
    let path_str = entry.path.to_string_lossy();
    let bufnr: i64 = if path_str.is_empty() {
        0
    } else {
        app.nvim_buffer_id_for_name(&path_str).unwrap_or(0) as i64
    };
    Value::Map(vec![
        (Value::from("bufnr"), Value::from(bufnr)),
        (Value::from("lnum"), Value::from((entry.row + 1) as i64)),
        (Value::from("col"), Value::from((entry.col + 1) as i64)),
        (Value::from("text"), Value::from(entry.message.as_str())),
        (Value::from("valid"), Value::from(1i64)),
    ])
}

/// Extract a value from a `Value::Map` by string key.
fn map_get<'a>(map: &'a [(Value, Value)], key: &str) -> Option<&'a Value> {
    map.iter().find_map(|(k, v)| {
        if let Value::String(s) = k
            && s.as_str() == Some(key)
        {
            return Some(v);
        }
        None
    })
}

/// Parse `fn_args[list_idx]` as a list of dicts and convert to `Vec<QfEntry>`.
/// Used by both `setqflist` and `setloclist`.
fn parse_qf_list(fn_args: &[Value], list_idx: usize, app: &crate::app::App) -> Vec<QfEntry> {
    let list = match fn_args.get(list_idx) {
        Some(Value::Array(arr)) => arr.as_slice(),
        _ => return Vec::new(),
    };

    list.iter()
        .filter_map(|v| {
            let map = match v {
                Value::Map(m) => m.as_slice(),
                _ => return None,
            };

            // path: from "filename" or "bufnr"
            let path = if let Some(Value::String(s)) = map_get(map, "filename") {
                PathBuf::from(s.as_str().unwrap_or(""))
            } else if let Some(Value::Integer(n)) = map_get(map, "bufnr") {
                let id = n.as_u64().unwrap_or(0);
                if id > 0 {
                    if let Some(name) = app.nvim_buffer_name(id) {
                        PathBuf::from(name)
                    } else {
                        PathBuf::new()
                    }
                } else {
                    PathBuf::new()
                }
            } else {
                PathBuf::new()
            };

            // row: lnum (1-based in dict) → 0-based; default 0. Clamp
            // negatives to 0 BEFORE the usize cast so a hostile lnum like -5
            // can't wrap into a huge row.
            let row = match map_get(map, "lnum") {
                Some(Value::Integer(n)) => {
                    (n.as_i64().unwrap_or(0).max(0) as usize).saturating_sub(1)
                }
                _ => 0,
            };

            // col: col (1-based in dict) → 0-based; default 0. Same clamp.
            let col = match map_get(map, "col") {
                Some(Value::Integer(n)) => {
                    (n.as_i64().unwrap_or(0).max(0) as usize).saturating_sub(1)
                }
                _ => 0,
            };

            // kind: "type" field — "W"->Warning, "I"->Info, "N"->Note, else Error
            let kind = match map_get(map, "type") {
                Some(Value::String(s)) => match s.as_str().unwrap_or("") {
                    "W" => QfKind::Warning,
                    "I" => QfKind::Info,
                    "N" => QfKind::Note,
                    _ => QfKind::Error,
                },
                _ => QfKind::Error,
            };

            // message: "text" field
            let message = match map_get(map, "text") {
                Some(Value::String(s)) => s.as_str().unwrap_or("").to_owned(),
                _ => String::new(),
            };

            Some(QfEntry {
                path,
                row,
                col,
                kind,
                message,
            })
        })
        .collect()
}

/// Expand a vimscript `%`-style filename expression against the current buffer.
fn expand_expr(app: &crate::app::App, expr: &str) -> String {
    // Resolve the current filename (use the stored path as-is first, then
    // fall back to the canonical name string used elsewhere).
    let filename = app.active().filename.clone();

    // Alternate file "#" — not tracked yet.
    if expr == "#" {
        return String::new();
    }

    let path: std::path::PathBuf = match &filename {
        Some(p) => p.clone(),
        None => {
            // Try via nvim_buffer_name which may have been set explicitly.
            let cur_id = app.nvim_current_buffer_id();
            match app.nvim_buffer_name(cur_id) {
                Some(s) if !s.is_empty() => PathBuf::from(s),
                _ => return String::new(),
            }
        }
    };

    match expr {
        "%" => path.to_string_lossy().into_owned(),
        "%:p" => {
            // Absolute path
            std::fs::canonicalize(&path)
                .unwrap_or_else(|_| {
                    if path.is_absolute() {
                        path.clone()
                    } else {
                        std::env::current_dir()
                            .map(|d| d.join(&path))
                            .unwrap_or_else(|_| path.clone())
                    }
                })
                .to_string_lossy()
                .into_owned()
        }
        "%:t" => path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
        "%:h" => path
            .parent()
            .map(|p| {
                let s = p.to_string_lossy();
                if s.is_empty() {
                    ".".to_owned()
                } else {
                    s.into_owned()
                }
            })
            .unwrap_or_else(|| ".".to_owned()),
        "%:e" => path
            .extension()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
        "%:r" => {
            // Remove the final extension — but only from the last path
            // component, and never a leading dot (a dotfile like `.bashrc`
            // has no extension). A dot inside a directory name, e.g.
            // `foo.bar/baz`, must be left untouched.
            let s = path.to_string_lossy();
            let file_start = s.rfind('/').map(|i| i + 1).unwrap_or(0);
            match s[file_start..].rfind('.') {
                // Dot strictly after the component start (relative index > 0).
                Some(rel) if rel > 0 => s[..file_start + rel].to_owned(),
                _ => s.into_owned(),
            }
        }
        _ => String::new(),
    }
}

// ── option coercion ───────────────────────────────────────────────────────────

/// Coerce a `:set`-style display string to the native nvim `Value` type.
///
/// Rules:
/// - `"on"`  → `Value::Boolean(true)`
/// - `"off"` → `Value::Boolean(false)`
/// - All-digit string (optionally with a leading `-`) → `Value::Integer`
/// - Starts and ends with `"` → strip the outer quotes → `Value::String`
/// - Otherwise → `Value::String` (the string as-is)
fn coerce_option_display(display: &str) -> Value {
    match display {
        "on" => return Value::Boolean(true),
        "off" => return Value::Boolean(false),
        _ => {}
    }
    // Integer: optional leading '-' then one or more ASCII digits.
    let digit_part = display.strip_prefix('-').unwrap_or(display);
    if !digit_part.is_empty()
        && digit_part.chars().all(|c| c.is_ascii_digit())
        && let Ok(n) = display.parse::<i64>()
    {
        return Value::Integer(rmpv::Integer::from(n));
    }
    // Quoted string: `"..."` → strip outer quotes.
    if display.starts_with('"') && display.ends_with('"') && display.len() >= 2 {
        let inner = &display[1..display.len() - 1];
        return Value::from(inner);
    }
    // Fallback: raw string value.
    Value::from(display)
}

// ── termcode translation ──────────────────────────────────────────────────────

/// Translate common vim key-notation tags to their control-byte equivalents.
///
/// Covered subset (case-insensitive tag names):
///   <CR> / <Enter> / <Return> → \r (0x0D)
///   <Esc>                     → \x1B
///   <Tab>                     → \t  (0x09)
///   <BS>                      → \x08
///   <Space>                   → ' ' (0x20)
///   <Nul>                     → \x00
///   <lt>                      → '<'
///   <Bar>                     → '|'
///   <Bslash>                  → '\'
///   <C-x> (any letter x)      → x & 0x1F  (ctrl byte)
///
/// Unrecognised `<...>` tags are left unchanged in the output.
/// This covers the common subset used by plugin configs; it does NOT implement
/// the full nvim termcode table (function keys, mouse, <M-x>, <A-x>, etc.).
fn replace_termcodes(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut chars = src.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '<' {
            out.push(ch);
            continue;
        }
        // Collect tag contents up to the matching '>'.
        let mut tag = String::new();
        let mut closed = false;
        for next in chars.by_ref() {
            if next == '>' {
                closed = true;
                break;
            }
            tag.push(next);
        }
        if !closed {
            // No closing '>' — emit literally and stop.
            out.push('<');
            out.push_str(&tag);
            break;
        }
        let lower = tag.to_ascii_lowercase();
        match lower.as_str() {
            "cr" | "enter" | "return" => out.push('\r'),
            "esc" => out.push('\x1B'),
            "tab" => out.push('\t'),
            "bs" => out.push('\x08'),
            "space" => out.push(' '),
            "nul" => out.push('\x00'),
            "lt" => out.push('<'),
            "bar" => out.push('|'),
            "bslash" => out.push('\\'),
            _ if lower.starts_with("c-") && lower.len() == 3 => {
                // <C-x> for a single ASCII letter → ctrl byte (x & 0x1F).
                let letter = lower.as_bytes()[2];
                if letter.is_ascii_alphabetic() || letter.is_ascii_digit() {
                    out.push((letter & 0x1F) as char);
                } else {
                    // Non-letter <C-x>: leave as-is.
                    out.push('<');
                    out.push_str(&tag);
                    out.push('>');
                }
            }
            _ => {
                // Unrecognised tag — leave as-is.
                out.push('<');
                out.push_str(&tag);
                out.push('>');
            }
        }
    }
    out
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

        "nvim_get_current_win" => ok(stdout, msgid, win_handle(app.nvim_current_window_id())),

        "nvim_list_wins" => {
            let handles: Vec<Value> = app.nvim_window_ids().into_iter().map(win_handle).collect();
            ok(stdout, msgid, Value::Array(handles))
        }

        "nvim_set_current_win" => {
            let p = match as_array(params) {
                Ok(p) => p,
                Err(e) => return err(stdout, msgid, &e),
            };
            let id = match param_win(p, 0) {
                Ok(Some(id)) => id,
                Ok(None) => app.nvim_current_window_id(),
                Err(e) => return err(stdout, msgid, &e),
            };
            if app.nvim_set_focused_window_checked(id) {
                settle(app);
                ok(stdout, msgid, Value::Nil)
            } else {
                err(stdout, msgid, "invalid window id")
            }
        }

        "nvim_win_get_buf" => {
            let p = match as_array(params) {
                Ok(p) => p,
                Err(e) => return err(stdout, msgid, &e),
            };
            let id = match param_win(p, 0) {
                Ok(Some(id)) => id,
                Ok(None) => app.nvim_current_window_id(),
                Err(e) => return err(stdout, msgid, &e),
            };
            match app.nvim_window_buffer_id(id) {
                Some(buf_id) => ok(stdout, msgid, buf_handle(buf_id)),
                None => err(stdout, msgid, "invalid window id"),
            }
        }

        "nvim_win_set_buf" => {
            let p = match as_array(params) {
                Ok(p) => p,
                Err(e) => return err(stdout, msgid, &e),
            };
            let win = match param_win(p, 0) {
                Ok(Some(id)) => id,
                Ok(None) => app.nvim_current_window_id(),
                Err(e) => return err(stdout, msgid, &e),
            };
            let buf = match param_buf(p, 1) {
                Ok(Some(id)) => id,
                Ok(None) => app.nvim_current_buffer_id(),
                Err(e) => return err(stdout, msgid, &e),
            };
            if app.nvim_set_window_buffer(win, buf) {
                settle(app);
                ok(stdout, msgid, Value::Nil)
            } else {
                err(stdout, msgid, "invalid window or buffer id")
            }
        }

        "nvim_win_close" => {
            let p = match as_array(params) {
                Ok(p) => p,
                Err(e) => return err(stdout, msgid, &e),
            };
            let id = match param_win(p, 0) {
                Ok(Some(id)) => id,
                Ok(None) => app.nvim_current_window_id(),
                Err(e) => return err(stdout, msgid, &e),
            };
            // Guard: do not close the last window.
            if app.nvim_window_ids().len() <= 1 {
                return ok(stdout, msgid, Value::Nil);
            }
            if !app.nvim_window_is_valid(id) {
                return err(stdout, msgid, "invalid window id");
            }
            // Focus the target window first if it isn't already focused.
            if app.nvim_current_window_id() != id {
                app.nvim_set_focused_window_checked(id);
            }
            app.close_focused_window();
            settle(app);
            ok(stdout, msgid, Value::Nil)
        }

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
            // Drain the content-reset queue `set_content` just filled and feed
            // it through the same syntax + LSP + dirty pipeline the keystroke
            // and ex-command paths use — `settle()` alone never drains it
            // (audit R2, fix 1). Look up the slot fresh: it's correct for both
            // the fast (current-buffer) and non-current branches above.
            if let Some(idx) = app.nvim_slot_index_for_buffer(buf_id) {
                app.sync_after_direct_content_mutation(idx);
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
            // params[0]=win handle, params[1]=[row, col]
            let win_id = match param_win(p, 0) {
                Ok(Some(id)) => id,
                Ok(None) => app.nvim_current_window_id(),
                Err(e) => return err(stdout, msgid, &e),
            };
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
            // Convert 1-based nvim row to 0-based engine row (saturating —
            // a hostile row of i64::MIN must not overflow the subtraction).
            let row = row_1based.saturating_sub(1).max(0) as usize;
            let current_win = app.nvim_current_window_id();
            if win_id == current_win {
                // Fast path: active editor (oracle-parity path).
                let char_col = {
                    let rope = app.active_editor().buffer().rope();
                    if row < rope.len_lines() {
                        let line = hjkl_buffer::rope_line_str(&rope, row);
                        byte_col_to_char_col(&line, col)
                    } else {
                        0
                    }
                };
                app.active_editor_mut().jump_cursor(row, char_col);
            } else {
                // Non-focused window: get rope from window's buffer for byte→char.
                let char_col = match app.nvim_window_cursor(win_id) {
                    Some(_) => {
                        // Determine rope for this window's buffer.
                        let buf_id = app.nvim_window_buffer_id(win_id);
                        let rope = if let Some(bid) = buf_id {
                            let current_id = app.nvim_current_buffer_id();
                            if bid == current_id {
                                app.active_editor().buffer().rope()
                            } else if let Some(ed) = app.nvim_slot_editor(bid) {
                                ed.buffer().rope()
                            } else {
                                app.active_editor().buffer().rope()
                            }
                        } else {
                            app.active_editor().buffer().rope()
                        };
                        if row < rope.len_lines() {
                            let line = hjkl_buffer::rope_line_str(&rope, row);
                            byte_col_to_char_col(&line, col)
                        } else {
                            0
                        }
                    }
                    None => return err(stdout, msgid, "invalid window id"),
                };
                if !app.nvim_set_window_cursor(win_id, row, char_col) {
                    return err(stdout, msgid, "invalid window id");
                }
            }
            settle(app);
            ok(stdout, msgid, Value::Nil)
        }

        "nvim_win_get_cursor" => {
            // nvim_win_get_cursor(win) — Returns [row (1-based), col (byte-col)].
            let p = match as_array(params) {
                Ok(p) => p,
                Err(e) => return err(stdout, msgid, &e),
            };
            let win_id = match param_win(p, 0) {
                Ok(Some(id)) => id,
                Ok(None) => app.nvim_current_window_id(),
                Err(e) => return err(stdout, msgid, &e),
            };
            let current_win = app.nvim_current_window_id();
            let (row, char_col) = if win_id == current_win {
                // Oracle-parity path: use active editor directly.
                app.active_editor().cursor()
            } else {
                match app.nvim_window_cursor(win_id) {
                    Some(c) => c,
                    None => return err(stdout, msgid, "invalid window id"),
                }
            };
            // Convert char-col to byte-col using the window's buffer rope.
            let byte_col = {
                let buf_id = app.nvim_window_buffer_id(win_id);
                let rope = if win_id == current_win {
                    app.active_editor().buffer().rope()
                } else if let Some(bid) = buf_id {
                    let current_id = app.nvim_current_buffer_id();
                    if bid == current_id {
                        app.active_editor().buffer().rope()
                    } else if let Some(ed) = app.nvim_slot_editor(bid) {
                        ed.buffer().rope()
                    } else {
                        app.active_editor().buffer().rope()
                    }
                } else {
                    app.active_editor().buffer().rope()
                };
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
            // Use the canonical post-edit sync, NOT settle (#262): settle's
            // reconcile_window_editors rebuilds the focused window editor from
            // its slot when the content Arc diverges, resetting the cursor to
            // the slot's stale position (the edit landed on the window editor).
            // sync_after_engine_mutation mirrors the TUI keypress path —
            // scrolloff / viewport / syntax sync without a window rebuild.
            app.sync_after_engine_mutation();
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

        // ── vimscript functions via nvim_call_function ────────────────────────
        "nvim_call_function" => {
            let p = match as_array(params) {
                Ok(p) => p,
                Err(e) => return err(stdout, msgid, &e),
            };
            let fn_name = match param_str(p, 0) {
                Ok(s) => s,
                Err(e) => return err(stdout, msgid, &e),
            };
            // params[1] is the argument array for the function.
            let fn_args: &[Value] = match p.get(1) {
                Some(Value::Array(arr)) => arr.as_slice(),
                _ => &[],
            };

            match fn_name.as_str() {
                // ── getreg ────────────────────────────────────────────────────
                "getreg" => {
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

                // ── getqflist ─────────────────────────────────────────────────
                "getqflist" => {
                    let entries: Vec<Value> = app
                        .quickfix
                        .entries()
                        .iter()
                        .map(|e| qf_entry_to_value(app, e))
                        .collect();
                    ok(stdout, msgid, Value::Array(entries))
                }

                // ── getloclist ────────────────────────────────────────────────
                "getloclist" => {
                    // args[0] = window (ignored)
                    let entries: Vec<Value> = app
                        .loclist
                        .entries()
                        .iter()
                        .map(|e| qf_entry_to_value(app, e))
                        .collect();
                    ok(stdout, msgid, Value::Array(entries))
                }

                // ── setqflist ─────────────────────────────────────────────────
                "setqflist" => {
                    // args[0] = list of dicts; optional args[1]=action, args[2]=what ignored
                    let qf_entries = parse_qf_list(fn_args, 0, app);
                    app.qf_push_history(crate::app::quickfix::QfWhich::Quickfix);
                    app.quickfix.set(qf_entries);
                    ok(stdout, msgid, Value::from(0i64))
                }

                // ── setloclist ────────────────────────────────────────────────
                "setloclist" => {
                    // args[0] = window (ignored); args[1] = list of dicts
                    let qf_entries = parse_qf_list(fn_args, 1, app);
                    app.qf_push_history(crate::app::quickfix::QfWhich::Location);
                    app.loclist.set(qf_entries);
                    ok(stdout, msgid, Value::from(0i64))
                }

                // ── bufnr ─────────────────────────────────────────────────────
                "bufnr" => {
                    let result = match fn_args.first() {
                        None => app.nvim_current_buffer_id() as i64,
                        Some(Value::String(s)) => {
                            let s = s.as_str().unwrap_or("");
                            if s.is_empty() || s == "%" {
                                app.nvim_current_buffer_id() as i64
                            } else if s == "$" {
                                app.nvim_buffer_ids().into_iter().max().unwrap_or(0) as i64
                            } else {
                                // substring match on buffer name
                                match app.nvim_buffer_id_for_name(s) {
                                    Some(id) => id as i64,
                                    None => -1,
                                }
                            }
                        }
                        Some(Value::Integer(n)) => {
                            let id = n.as_u64().unwrap_or(0);
                            if app.nvim_slot_index_for_buffer(id).is_some() {
                                id as i64
                            } else {
                                -1
                            }
                        }
                        _ => -1,
                    };
                    ok(stdout, msgid, Value::from(result))
                }

                // ── bufname ───────────────────────────────────────────────────
                "bufname" => {
                    let name = match fn_args.first() {
                        None => app
                            .nvim_buffer_name(app.nvim_current_buffer_id())
                            .unwrap_or_default(),
                        Some(Value::String(s)) => {
                            let s = s.as_str().unwrap_or("");
                            if s.is_empty() || s == "%" {
                                app.nvim_buffer_name(app.nvim_current_buffer_id())
                                    .unwrap_or_default()
                            } else {
                                match app.nvim_buffer_id_for_name(s) {
                                    Some(id) => app.nvim_buffer_name(id).unwrap_or_default(),
                                    None => String::new(),
                                }
                            }
                        }
                        Some(Value::Integer(n)) => {
                            let id = n.as_u64().unwrap_or(0);
                            let cid = if id == 0 {
                                app.nvim_current_buffer_id()
                            } else {
                                id
                            };
                            app.nvim_buffer_name(cid).unwrap_or_default()
                        }
                        _ => String::new(),
                    };
                    ok(stdout, msgid, Value::from(name.as_str()))
                }

                // ── expand ────────────────────────────────────────────────────
                "expand" => {
                    let expr = match fn_args.first() {
                        Some(Value::String(s)) => s.as_str().unwrap_or("").to_owned(),
                        _ => String::new(),
                    };
                    let result = expand_expr(app, &expr);
                    ok(stdout, msgid, Value::from(result.as_str()))
                }

                // ── line ──────────────────────────────────────────────────────
                "line" => {
                    let expr = match fn_args.first() {
                        Some(Value::String(s)) => s.as_str().unwrap_or(".").to_owned(),
                        _ => ".".to_owned(),
                    };
                    let rope = app.active_editor().buffer().rope();
                    let result: i64 = match expr.as_str() {
                        "." => {
                            let (row, _) = app.active_editor().cursor();
                            (row + 1) as i64
                        }
                        "$" => {
                            let n = rope.len_lines();
                            // vim: empty buffer has 1 line
                            n.max(1) as i64
                        }
                        // "v" → anchor row when in charwise Visual, else cursor row
                        "v" => {
                            let ed = app.active_editor();
                            let row = if ed.vim_mode() == hjkl_engine::VimMode::Visual {
                                ed.visual_anchor().0
                            } else {
                                ed.cursor().0
                            };
                            (row + 1) as i64
                        }
                        _ => 0,
                    };
                    ok(stdout, msgid, Value::from(result))
                }

                // ── col ───────────────────────────────────────────────────────
                "col" => {
                    let expr = match fn_args.first() {
                        Some(Value::String(s)) => s.as_str().unwrap_or(".").to_owned(),
                        _ => ".".to_owned(),
                    };
                    let (row, char_col) = app.active_editor().cursor();
                    let rope = app.active_editor().buffer().rope();
                    let result: i64 = match expr.as_str() {
                        "." => (char_col + 1) as i64,
                        "$" => {
                            // Length of the current line in chars + 1
                            let line = hjkl_buffer::rope_line_str(&rope, row);
                            (line.chars().count() + 1) as i64
                        }
                        // "v" → anchor char-col when in charwise Visual, else cursor col.
                        // Matches the char-col convention used by col(".") throughout
                        // this file (1-based char-col, NOT byte-col).
                        "v" => {
                            let ed = app.active_editor();
                            let col = if ed.vim_mode() == hjkl_engine::VimMode::Visual {
                                ed.visual_anchor().1
                            } else {
                                char_col
                            };
                            (col + 1) as i64
                        }
                        _ => 0,
                    };
                    ok(stdout, msgid, Value::from(result))
                }

                // ── getpos ────────────────────────────────────────────────────
                // Returns nvim's 4-element [bufnum, lnum, col, off] form
                // (1-based lnum and col, bufnum=0, off=0).
                // Col follows the same char-col convention as col(".") above —
                // 1-based char index, NOT byte offset. Supported exprs:
                //   "."  → cursor
                //   "v"  → visual anchor when in charwise Visual; cursor otherwise
                //          (nvim fallback when not in visual)
                //   "'<" / "'>": last-visual-start / end — not tracked; returns cursor
                "getpos" => {
                    let expr = match fn_args.first() {
                        Some(Value::String(s)) => s.as_str().unwrap_or(".").to_owned(),
                        _ => ".".to_owned(),
                    };
                    let ed = app.active_editor();
                    let (cur_row, cur_col) = ed.cursor();
                    let (lnum, col): (usize, usize) = match expr.as_str() {
                        "v" => {
                            if ed.vim_mode() == hjkl_engine::VimMode::Visual {
                                ed.visual_anchor()
                            } else {
                                (cur_row, cur_col)
                            }
                        }
                        // "'<" and "'>" are not yet tracked; fall back to cursor
                        // position as nvim does when no prior visual selection exists.
                        _ => (cur_row, cur_col),
                    };
                    let pos = Value::Array(vec![
                        Value::from(0i64),              // bufnum
                        Value::from((lnum + 1) as i64), // 1-based row
                        Value::from((col + 1) as i64),  // 1-based char-col
                        Value::from(0i64),              // off
                    ]);
                    ok(stdout, msgid, pos)
                }

                _ => err(
                    stdout,
                    msgid,
                    &format!("nvim_call_function: unsupported function: {fn_name}"),
                ),
            }
        }

        // ── tabpage API ───────────────────────────────────────────────────────
        "nvim_list_tabpages" => {
            let handles: Vec<Value> = (0..app.tabs.len() as u64).map(tab_handle).collect();
            ok(stdout, msgid, Value::Array(handles))
        }

        "nvim_get_current_tabpage" => ok(stdout, msgid, tab_handle(app.active_tab as u64)),

        "nvim_set_current_tabpage" => {
            let p = match as_array(params) {
                Ok(p) => p,
                Err(e) => return err(stdout, msgid, &e),
            };
            let id = match param_tabpage(p, 0) {
                Ok(Some(id)) => id,
                Ok(None) => app.active_tab as u64,
                Err(e) => return err(stdout, msgid, &e),
            };
            if id as usize >= app.tabs.len() {
                return err(stdout, msgid, "invalid tabpage");
            }
            app.switch_tab(id as usize);
            settle(app);
            ok(stdout, msgid, Value::Nil)
        }

        "nvim_tabpage_list_wins" => {
            let p = match as_array(params) {
                Ok(p) => p,
                Err(e) => return err(stdout, msgid, &e),
            };
            let id = match param_tabpage(p, 0) {
                Ok(Some(id)) => id,
                Ok(None) => app.active_tab as u64,
                Err(e) => return err(stdout, msgid, &e),
            };
            match app.nvim_tab_window_ids(id as usize) {
                Some(wins) => {
                    let handles: Vec<Value> = wins.into_iter().map(win_handle).collect();
                    ok(stdout, msgid, Value::Array(handles))
                }
                None => err(stdout, msgid, "invalid tabpage"),
            }
        }

        "nvim_tabpage_is_valid" => {
            let p = match as_array(params) {
                Ok(p) => p,
                Err(e) => return err(stdout, msgid, &e),
            };
            let id = match param_tabpage(p, 0) {
                Ok(Some(id)) => id,
                Ok(None) => app.active_tab as u64,
                Err(e) => return err(stdout, msgid, &e),
            };
            ok(
                stdout,
                msgid,
                Value::Boolean((id as usize) < app.tabs.len()),
            )
        }

        // ── buffer line count / current line / byte-range text ────────────────
        "nvim_buf_line_count" => {
            let p = match as_array(params) {
                Ok(p) => p,
                Err(e) => return err(stdout, msgid, &e),
            };
            let buf_id = match param_buf(p, 0) {
                Ok(Some(id)) => id,
                Ok(None) => app.nvim_current_buffer_id(),
                Err(e) => return err(stdout, msgid, &e),
            };
            let current_id = app.nvim_current_buffer_id();
            let rope = if buf_id == current_id {
                app.active_editor().buffer().rope()
            } else {
                match app.nvim_slot_editor(buf_id) {
                    Some(ed) => ed.buffer().rope(),
                    None => return err(stdout, msgid, "invalid buffer id"),
                }
            };
            // Match nvim semantics: an empty buffer has 1 line.
            // ropey's len_lines() returns 1 for an empty rope, and for
            // "a\nb" it returns 2, matching nvim_buf_get_lines(0,-1).len().
            let count = rope.len_lines().max(1) as i64;
            ok(stdout, msgid, Value::from(count))
        }

        "nvim_get_current_line" => {
            let (row, _) = app.active_editor().cursor();
            let rope = app.active_editor().buffer().rope();
            let line = if row < rope.len_lines() {
                hjkl_buffer::rope_line_str(&rope, row)
            } else {
                String::new()
            };
            ok(stdout, msgid, Value::from(line.as_str()))
        }

        "nvim_set_current_line" => {
            let p = match as_array(params) {
                Ok(p) => p,
                Err(e) => return err(stdout, msgid, &e),
            };
            let new_line = match param_str(p, 0) {
                Ok(s) => s,
                Err(e) => return err(stdout, msgid, &e),
            };
            let (row, _) = app.active_editor().cursor();
            let rope = app.active_editor().buffer().rope();
            let line_count = rope.len_lines();

            // Rebuild full content with the current row replaced.
            let mut result: Vec<String> = Vec::with_capacity(line_count);
            for i in 0..line_count {
                if i == row {
                    result.push(new_line.clone());
                } else {
                    result.push(hjkl_buffer::rope_line_str(&rope, i));
                }
            }
            let content = result.join("\n");
            app.active_editor_mut().set_content(&content);
            settle(app);
            ok(stdout, msgid, Value::Nil)
        }

        "nvim_buf_get_text" => {
            // nvim_buf_get_text(buf, start_row, start_col, end_row, end_col, opts)
            // Rows 0-based; cols are BYTE offsets within the line.
            // Returns Array of strings, one per spanned line (first/last clipped).
            let p = match as_array(params) {
                Ok(p) => p,
                Err(e) => return err(stdout, msgid, &e),
            };
            let buf_id = match param_buf(p, 0) {
                Ok(Some(id)) => id,
                Ok(None) => app.nvim_current_buffer_id(),
                Err(e) => return err(stdout, msgid, &e),
            };
            let start_row = match param_i64(p, 1) {
                Ok(v) => v.max(0) as usize,
                Err(e) => return err(stdout, msgid, &e),
            };
            let start_col = match param_i64(p, 2) {
                Ok(v) => v.max(0) as usize,
                Err(e) => return err(stdout, msgid, &e),
            };
            let end_row = match param_i64(p, 3) {
                Ok(v) => v.max(0) as usize,
                Err(e) => return err(stdout, msgid, &e),
            };
            let end_col = match param_i64(p, 4) {
                Ok(v) => v.max(0) as usize,
                Err(e) => return err(stdout, msgid, &e),
            };
            // params[5] = opts dict — ignored

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
            let start_row = start_row.min(line_count.saturating_sub(1));
            let end_row = end_row.min(line_count.saturating_sub(1));

            let mut result: Vec<Value> = Vec::new();
            for row in start_row..=end_row {
                let line = hjkl_buffer::rope_line_str(&rope, row);
                let bytes = line.as_bytes();
                let (lo, hi) = if start_row == end_row {
                    // Single-row range: clip both ends
                    (start_col.min(bytes.len()), end_col.min(bytes.len()))
                } else if row == start_row {
                    (start_col.min(bytes.len()), bytes.len())
                } else if row == end_row {
                    (0, end_col.min(bytes.len()))
                } else {
                    (0, bytes.len())
                };
                // Ensure lo/hi are on valid UTF-8 char boundaries.
                let lo = (0..=lo.min(bytes.len()))
                    .rev()
                    .find(|&i| line.is_char_boundary(i))
                    .unwrap_or(0);
                let hi = (hi.min(bytes.len())..=bytes.len())
                    .find(|&i| line.is_char_boundary(i))
                    .unwrap_or(bytes.len());
                result.push(Value::from(&line[lo..hi]));
            }
            ok(stdout, msgid, Value::Array(result))
        }

        "nvim_buf_set_text" => {
            // nvim_buf_set_text(buf, start_row, start_col, end_row, end_col, replacement)
            // Rows 0-based; cols are BYTE offsets. replacement is String[].
            //
            // NOTE: This implementation materialises the whole buffer to a String,
            // splices the replacement, then writes it back via set_content. This
            // resets undo history — acceptable for headless API v1.
            let p = match as_array(params) {
                Ok(p) => p,
                Err(e) => return err(stdout, msgid, &e),
            };
            let buf_id = match param_buf(p, 0) {
                Ok(Some(id)) => id,
                Ok(None) => app.nvim_current_buffer_id(),
                Err(e) => return err(stdout, msgid, &e),
            };
            let start_row = match param_i64(p, 1) {
                Ok(v) => v.max(0) as usize,
                Err(e) => return err(stdout, msgid, &e),
            };
            let start_col = match param_i64(p, 2) {
                Ok(v) => v.max(0) as usize,
                Err(e) => return err(stdout, msgid, &e),
            };
            let end_row = match param_i64(p, 3) {
                Ok(v) => v.max(0) as usize,
                Err(e) => return err(stdout, msgid, &e),
            };
            let end_col = match param_i64(p, 4) {
                Ok(v) => v.max(0) as usize,
                Err(e) => return err(stdout, msgid, &e),
            };
            let replacement = match param_string_array(p, 5) {
                Ok(v) => v,
                Err(e) => return err(stdout, msgid, &e),
            };

            let current_id = app.nvim_current_buffer_id();
            let is_current = buf_id == current_id;

            // Materialise buffer lines.
            let lines: Vec<String> = {
                let rope = if is_current {
                    app.active_editor().buffer().rope()
                } else {
                    match app.nvim_slot_editor(buf_id) {
                        Some(ed) => ed.buffer().rope(),
                        None => return err(stdout, msgid, "invalid buffer id"),
                    }
                };
                let n = rope.len_lines();
                (0..n)
                    .map(|i| hjkl_buffer::rope_line_str(&rope, i))
                    .collect()
            };

            // Compute absolute byte positions by walking lines.
            // Each line contributes its byte length + 1 for the joining '\n'.
            let line_start_byte = |row: usize| -> usize {
                lines[..row.min(lines.len())]
                    .iter()
                    .map(|l| l.len() + 1)
                    .sum()
            };

            let full = lines.join("\n");
            let full_len = full.len();

            let abs_start = {
                let ls = line_start_byte(start_row);
                let line_bytes = lines.get(start_row).map(|l| l.len()).unwrap_or(0);
                let col = start_col.min(line_bytes);
                // Snap to valid UTF-8 boundary.
                let s = ls + col;
                (0..=s.min(full_len))
                    .rev()
                    .find(|&i| full.is_char_boundary(i))
                    .unwrap_or(0)
            };
            let abs_end = {
                let ls = line_start_byte(end_row);
                let line_bytes = lines.get(end_row).map(|l| l.len()).unwrap_or(0);
                let col = end_col.min(line_bytes);
                let s = ls + col;
                let s = s.min(full_len);
                (s..=full_len)
                    .find(|&i| full.is_char_boundary(i))
                    .unwrap_or(full_len)
            };

            // Reject an inverted range (start after end) like nvim does —
            // splicing with abs_start > abs_end would silently duplicate the
            // bytes between the two positions.
            if abs_start > abs_end {
                return err(stdout, msgid, "start is higher than end");
            }

            let new_text = replacement.join("\n");
            let new_content = format!("{}{}{}", &full[..abs_start], new_text, &full[abs_end..]);

            if is_current {
                app.active_editor_mut().set_content(&new_content);
            } else {
                match app.nvim_slot_editor_mut(buf_id) {
                    Some(ed) => ed.set_content(&new_content),
                    None => return err(stdout, msgid, "invalid buffer id"),
                }
            }
            // See the matching comment in nvim_buf_set_lines: `settle()` alone
            // never drains the content-reset/edit queue `set_content` just
            // filled (audit R2, fix 1).
            if let Some(idx) = app.nvim_slot_index_for_buffer(buf_id) {
                app.sync_after_direct_content_mutation(idx);
            }
            settle(app);
            ok(stdout, msgid, Value::Nil)
        }

        // ── window size accessors / mutators ──────────────────────────────────
        "nvim_win_get_height" => {
            let p = match as_array(params) {
                Ok(p) => p,
                Err(e) => return err(stdout, msgid, &e),
            };
            let win_id = match param_win(p, 0) {
                Ok(Some(id)) => id,
                Ok(None) => app.nvim_current_window_id(),
                Err(e) => return err(stdout, msgid, &e),
            };
            if !app.nvim_window_is_valid(win_id) {
                return err(stdout, msgid, "invalid window id");
            }
            let area = win_area(app);
            let layout = &app.tabs[app.active_tab].layout;
            match layout
                .window_rects(area)
                .into_iter()
                .find(|(id, _)| *id == win_id as usize)
            {
                Some((_, rect)) => ok(stdout, msgid, Value::from(rect.h as i64)),
                None => err(stdout, msgid, "window not found in active tab layout"),
            }
        }

        "nvim_win_get_width" => {
            let p = match as_array(params) {
                Ok(p) => p,
                Err(e) => return err(stdout, msgid, &e),
            };
            let win_id = match param_win(p, 0) {
                Ok(Some(id)) => id,
                Ok(None) => app.nvim_current_window_id(),
                Err(e) => return err(stdout, msgid, &e),
            };
            if !app.nvim_window_is_valid(win_id) {
                return err(stdout, msgid, "invalid window id");
            }
            let area = win_area(app);
            let layout = &app.tabs[app.active_tab].layout;
            match layout
                .window_rects(area)
                .into_iter()
                .find(|(id, _)| *id == win_id as usize)
            {
                Some((_, rect)) => ok(stdout, msgid, Value::from(rect.w as i64)),
                None => err(stdout, msgid, "window not found in active tab layout"),
            }
        }

        "nvim_win_set_height" => {
            // nvim_win_set_height(win, height)
            // Best-effort: find the enclosing Horizontal split and adjust its ratio
            // so the target window gets approximately `height` rows.
            // No-op (ok) when the window has no enclosing horizontal split.
            let p = match as_array(params) {
                Ok(p) => p,
                Err(e) => return err(stdout, msgid, &e),
            };
            let win_id = match param_win(p, 0) {
                Ok(Some(id)) => id,
                Ok(None) => app.nvim_current_window_id(),
                Err(e) => return err(stdout, msgid, &e),
            };
            let desired_h = match param_i64(p, 1) {
                Ok(v) => v,
                Err(e) => return err(stdout, msgid, &e),
            };
            if !app.nvim_window_is_valid(win_id) {
                return err(stdout, msgid, "invalid window id");
            }
            let area = win_area(app);
            let layout = &mut app.tabs[app.active_tab].layout;
            if let Some((ratio, _saved, in_a)) =
                layout.enclosing_split_mut(win_id as usize, SplitDir::Horizontal)
            {
                // The parent height comes from the full headless area for the
                // enclosing split. We use `area.h` as a conservative estimate
                // (the actual parent may be smaller in deeply nested layouts,
                // but for top-level splits this is exact).
                let parent_h = area.h as f32;
                let desired = (desired_h as f32).clamp(1.0, parent_h - 1.0);
                let new_ratio = if in_a {
                    desired / parent_h
                } else {
                    1.0 - desired / parent_h
                };
                *ratio = new_ratio.clamp(0.05, 0.95);
            }
            settle(app);
            ok(stdout, msgid, Value::Nil)
        }

        "nvim_win_set_width" => {
            // nvim_win_set_width(win, width)
            // Best-effort: find the enclosing Vertical split and adjust its ratio
            // so the target window gets approximately `width` columns.
            // No-op (ok) when the window has no enclosing vertical split.
            let p = match as_array(params) {
                Ok(p) => p,
                Err(e) => return err(stdout, msgid, &e),
            };
            let win_id = match param_win(p, 0) {
                Ok(Some(id)) => id,
                Ok(None) => app.nvim_current_window_id(),
                Err(e) => return err(stdout, msgid, &e),
            };
            let desired_w = match param_i64(p, 1) {
                Ok(v) => v,
                Err(e) => return err(stdout, msgid, &e),
            };
            if !app.nvim_window_is_valid(win_id) {
                return err(stdout, msgid, "invalid window id");
            }
            let area = win_area(app);
            let layout = &mut app.tabs[app.active_tab].layout;
            if let Some((ratio, _saved, in_a)) =
                layout.enclosing_split_mut(win_id as usize, SplitDir::Vertical)
            {
                let parent_w = area.w as f32;
                let desired = (desired_w as f32).clamp(1.0, parent_w - 1.0);
                let new_ratio = if in_a {
                    desired / parent_w
                } else {
                    1.0 - desired / parent_w
                };
                *ratio = new_ratio.clamp(0.05, 0.95);
            }
            settle(app);
            ok(stdout, msgid, Value::Nil)
        }

        // ── option get / set ──────────────────────────────────────────────────
        "nvim_get_option_value" => {
            // nvim_get_option_value(name: string, opts: dict|nil)
            // opts (scope/buf/win) are ignored in v1 — always operates on the
            // active editor.
            let p = match as_array(params) {
                Ok(p) => p,
                Err(e) => return err(stdout, msgid, &e),
            };
            let name = match param_str(p, 0) {
                Ok(s) => s,
                Err(e) => return err(stdout, msgid, &e),
            };
            // Reject unknown option names immediately.
            if !hjkl_ex::all_setting_names().contains(&name) {
                return err(stdout, msgid, &format!("unknown option: {name}"));
            }
            let display = match hjkl_ex::query_option_value(app.active_editor(), &name) {
                Some(s) => s,
                None => {
                    return err(stdout, msgid, &format!("unknown option: {name}"));
                }
            };
            // Coerce the display string to the native nvim type:
            //   "on"  → Boolean(true)
            //   "off" → Boolean(false)
            //   all-digit (optionally leading '-') → Integer
            //   starts and ends with '"' → strip quotes → String
            //   otherwise → String (as-is)
            let value = coerce_option_display(&display);
            ok(stdout, msgid, value)
        }

        "nvim_set_option_value" => {
            // nvim_set_option_value(name: string, value: any, opts: dict|nil)
            // opts (scope/buf/win) are ignored in v1 — always operates on the
            // active editor.
            let p = match as_array(params) {
                Ok(p) => p,
                Err(e) => return err(stdout, msgid, &e),
            };
            let name = match param_str(p, 0) {
                Ok(s) => s,
                Err(e) => return err(stdout, msgid, &e),
            };
            // Reject unknown option names.
            if !hjkl_ex::all_setting_names().contains(&name) {
                return err(stdout, msgid, &format!("unknown option: {name}"));
            }
            let value = match p.get(1) {
                Some(v) => v.clone(),
                None => return err(stdout, msgid, "params[1] (value) missing"),
            };
            // Build the `:set`-style token and apply it via apply_set_token.
            // We call apply_set_token directly (rather than routing through
            // dispatch_ex / apply_set) so that string values containing
            // whitespace (e.g. `makeprg=cargo build`) are treated as a single
            // token without being split by apply_set's split_whitespace loop.
            let set_token = match &value {
                Value::Boolean(true) => name.clone(),
                Value::Boolean(false) => format!("no{name}"),
                Value::Integer(n) => {
                    let n = n.as_i64().unwrap_or(0);
                    format!("{name}={n}")
                }
                Value::String(s) => {
                    let s = s.as_str().unwrap_or("");
                    format!("{name}={s}")
                }
                other => {
                    return err(
                        stdout,
                        msgid,
                        &format!("unsupported value type for nvim_set_option_value: {other:?}"),
                    );
                }
            };
            if let Err(e) = hjkl_ex::apply_set_token(app.active_editor_mut(), &set_token) {
                return err(stdout, msgid, &e);
            }
            settle(app);
            ok(stdout, msgid, Value::Nil)
        }

        // ── global variable store (g:) ────────────────────────────────────────
        "nvim_set_var" => {
            let p = match as_array(params) {
                Ok(p) => p,
                Err(e) => return err(stdout, msgid, &e),
            };
            let name = match param_str(p, 0) {
                Ok(s) => s,
                Err(e) => return err(stdout, msgid, &e),
            };
            let value = match p.get(1) {
                Some(v) => v.clone(),
                None => return err(stdout, msgid, "params[1] (value) missing"),
            };
            app.nvim_gvars.insert(name, value);
            ok(stdout, msgid, Value::Nil)
        }

        "nvim_get_var" => {
            let p = match as_array(params) {
                Ok(p) => p,
                Err(e) => return err(stdout, msgid, &e),
            };
            let name = match param_str(p, 0) {
                Ok(s) => s,
                Err(e) => return err(stdout, msgid, &e),
            };
            match app.nvim_gvars.get(&name) {
                Some(v) => ok(stdout, msgid, v.clone()),
                None => err(stdout, msgid, &format!("Key not found: {name}")),
            }
        }

        "nvim_del_var" => {
            let p = match as_array(params) {
                Ok(p) => p,
                Err(e) => return err(stdout, msgid, &e),
            };
            let name = match param_str(p, 0) {
                Ok(s) => s,
                Err(e) => return err(stdout, msgid, &e),
            };
            match app.nvim_gvars.remove(&name) {
                Some(_) => ok(stdout, msgid, Value::Nil),
                None => err(stdout, msgid, &format!("Key not found: {name}")),
            }
        }

        // ── buffer-local variable store (b:) ──────────────────────────────────
        "nvim_buf_set_var" => {
            let p = match as_array(params) {
                Ok(p) => p,
                Err(e) => return err(stdout, msgid, &e),
            };
            let buf_id = match param_buf(p, 0) {
                Ok(Some(id)) => id,
                Ok(None) => app.nvim_current_buffer_id(),
                Err(e) => return err(stdout, msgid, &e),
            };
            let name = match param_str(p, 1) {
                Ok(s) => s,
                Err(e) => return err(stdout, msgid, &e),
            };
            let value = match p.get(2) {
                Some(v) => v.clone(),
                None => return err(stdout, msgid, "params[2] (value) missing"),
            };
            app.nvim_bvars.insert((buf_id, name), value);
            ok(stdout, msgid, Value::Nil)
        }

        "nvim_buf_get_var" => {
            let p = match as_array(params) {
                Ok(p) => p,
                Err(e) => return err(stdout, msgid, &e),
            };
            let buf_id = match param_buf(p, 0) {
                Ok(Some(id)) => id,
                Ok(None) => app.nvim_current_buffer_id(),
                Err(e) => return err(stdout, msgid, &e),
            };
            let name = match param_str(p, 1) {
                Ok(s) => s,
                Err(e) => return err(stdout, msgid, &e),
            };
            match app.nvim_bvars.get(&(buf_id, name.clone())) {
                Some(v) => ok(stdout, msgid, v.clone()),
                None => err(stdout, msgid, &format!("Key not found: {name}")),
            }
        }

        "nvim_buf_del_var" => {
            let p = match as_array(params) {
                Ok(p) => p,
                Err(e) => return err(stdout, msgid, &e),
            };
            let buf_id = match param_buf(p, 0) {
                Ok(Some(id)) => id,
                Ok(None) => app.nvim_current_buffer_id(),
                Err(e) => return err(stdout, msgid, &e),
            };
            let name = match param_str(p, 1) {
                Ok(s) => s,
                Err(e) => return err(stdout, msgid, &e),
            };
            match app.nvim_bvars.remove(&(buf_id, name.clone())) {
                Some(_) => ok(stdout, msgid, Value::Nil),
                None => err(stdout, msgid, &format!("Key not found: {name}")),
            }
        }

        // ── window-local variable store (w:) ──────────────────────────────────
        "nvim_win_set_var" => {
            let p = match as_array(params) {
                Ok(p) => p,
                Err(e) => return err(stdout, msgid, &e),
            };
            let win_id = match param_win(p, 0) {
                Ok(Some(id)) => id,
                Ok(None) => app.nvim_current_window_id(),
                Err(e) => return err(stdout, msgid, &e),
            };
            let name = match param_str(p, 1) {
                Ok(s) => s,
                Err(e) => return err(stdout, msgid, &e),
            };
            let value = match p.get(2) {
                Some(v) => v.clone(),
                None => return err(stdout, msgid, "params[2] (value) missing"),
            };
            app.nvim_wvars.insert((win_id, name), value);
            ok(stdout, msgid, Value::Nil)
        }

        "nvim_win_get_var" => {
            let p = match as_array(params) {
                Ok(p) => p,
                Err(e) => return err(stdout, msgid, &e),
            };
            let win_id = match param_win(p, 0) {
                Ok(Some(id)) => id,
                Ok(None) => app.nvim_current_window_id(),
                Err(e) => return err(stdout, msgid, &e),
            };
            let name = match param_str(p, 1) {
                Ok(s) => s,
                Err(e) => return err(stdout, msgid, &e),
            };
            match app.nvim_wvars.get(&(win_id, name.clone())) {
                Some(v) => ok(stdout, msgid, v.clone()),
                None => err(stdout, msgid, &format!("Key not found: {name}")),
            }
        }

        "nvim_win_del_var" => {
            let p = match as_array(params) {
                Ok(p) => p,
                Err(e) => return err(stdout, msgid, &e),
            };
            let win_id = match param_win(p, 0) {
                Ok(Some(id)) => id,
                Ok(None) => app.nvim_current_window_id(),
                Err(e) => return err(stdout, msgid, &e),
            };
            let name = match param_str(p, 1) {
                Ok(s) => s,
                Err(e) => return err(stdout, msgid, &e),
            };
            match app.nvim_wvars.remove(&(win_id, name.clone())) {
                Some(_) => ok(stdout, msgid, Value::Nil),
                None => err(stdout, msgid, &format!("Key not found: {name}")),
            }
        }

        // ── keymap API ────────────────────────────────────────────────────────
        "nvim_set_keymap" => {
            // nvim_set_keymap(mode: string, lhs: string, rhs: string, opts: dict|nil)
            //
            // Maps the nvim single-char mode to the hjkl ex-command prefix,
            // then calls dispatch_ex("{prefix}{noremap|map} {lhs} {rhs}").
            //
            // Mode character → prefix mapping (confirmed from keymap.rs parse_mode_groups):
            //   "n"  → n   (nmap / nnoremap)
            //   "i"  → i   (imap / inoremap)
            //   "v"  → v   (vmap / vnoremap)
            //   "x"  → x   (xmap / xnoremap — also Visual in hjkl)
            //   "o"  → o   (omap / onoremap — OperatorPending)
            //   ""   →     (map / noremap — Normal+Visual+OpPending)
            //   any other → plain map (best-effort fallback)
            let p = match as_array(params) {
                Ok(p) => p,
                Err(e) => return err(stdout, msgid, &e),
            };
            let mode = match param_str(p, 0) {
                Ok(s) => s,
                Err(e) => return err(stdout, msgid, &e),
            };
            let lhs = match param_str(p, 1) {
                Ok(s) => s,
                Err(e) => return err(stdout, msgid, &e),
            };
            let rhs = match param_str(p, 2) {
                Ok(s) => s,
                Err(e) => return err(stdout, msgid, &e),
            };
            // Extract opts dict — noremap defaults to false.
            let noremap = match p.get(3) {
                Some(Value::Map(m)) => map_get(m, "noremap")
                    .and_then(|v| {
                        if let Value::Boolean(b) = v {
                            Some(*b)
                        } else {
                            None
                        }
                    })
                    .unwrap_or(false),
                _ => false,
            };
            let prefix = match mode.as_str() {
                "n" => "n",
                "i" => "i",
                "v" => "v",
                "x" => "x",
                "o" => "o",
                "" => "",
                _ => "", // unknown modes fall back to plain map
            };
            let verb = if noremap {
                if prefix.is_empty() {
                    "noremap".to_string()
                } else {
                    format!("{prefix}noremap")
                }
            } else if prefix.is_empty() {
                "map".to_string()
            } else {
                format!("{prefix}map")
            };
            let cmd = format!("{verb} {lhs} {rhs}");
            app.dispatch_ex(&cmd);
            settle(app);
            ok(stdout, msgid, Value::Nil)
        }

        "nvim_del_keymap" => {
            // nvim_del_keymap(mode: string, lhs: string)
            // Builds "{prefix}unmap {lhs}" and dispatches it.
            // Mode → unmap prefix mapping (from keymap.rs parse_mode_groups):
            //   "n" → nunmap, "i" → iunmap, "v" → vunmap, "x" → xunmap,
            //   "o" → ounmap, "" → unmap (Normal+Visual+OpPending+Insert+...)
            let p = match as_array(params) {
                Ok(p) => p,
                Err(e) => return err(stdout, msgid, &e),
            };
            let mode = match param_str(p, 0) {
                Ok(s) => s,
                Err(e) => return err(stdout, msgid, &e),
            };
            let lhs = match param_str(p, 1) {
                Ok(s) => s,
                Err(e) => return err(stdout, msgid, &e),
            };
            let prefix = match mode.as_str() {
                "n" => "n",
                "i" => "i",
                "v" => "v",
                "x" => "x",
                "o" => "o",
                _ => "", // "" and unknowns → plain unmap
            };
            let verb = format!("{prefix}unmap");
            let cmd = format!("{verb} {lhs}");
            app.dispatch_ex(&cmd);
            settle(app);
            ok(stdout, msgid, Value::Nil)
        }

        "nvim_replace_termcodes" => {
            // nvim_replace_termcodes(str: string, from_part: bool, do_lt: bool, special: bool)
            //
            // v1: translates the common subset of vim key-notation tags to their
            // control-byte equivalents. Unrecognised <...> tags are left as-is.
            // This does NOT cover the full nvim termcode table (function keys,
            // mouse events, modifiers beyond C-x, etc.).
            let p = match as_array(params) {
                Ok(p) => p,
                Err(e) => return err(stdout, msgid, &e),
            };
            let src = match param_str(p, 0) {
                Ok(s) => s,
                Err(e) => return err(stdout, msgid, &e),
            };
            // from_part / do_lt / special are accepted but not acted on in v1.
            let result = replace_termcodes(&src);
            ok(stdout, msgid, Value::from(result.as_str()))
        }

        "nvim_feedkeys" => {
            // nvim_feedkeys(keys: string, mode: string, escape_ks: bool)
            //
            // v1: replays `keys` through the same engine path as nvim_input —
            // decode_macro parses vim notation and dispatch_input feeds each
            // key to the active editor. The `mode` flags (remap/typeahead/etc.)
            // and `escape_ks` are NOT honoured in v1; all keys are dispatched
            // immediately in the current editor mode.
            let p = match as_array(params) {
                Ok(p) => p,
                Err(e) => return err(stdout, msgid, &e),
            };
            let keys = match param_str(p, 0) {
                Ok(s) => s,
                Err(e) => return err(stdout, msgid, &e),
            };
            // params[1]=mode, params[2]=escape_ks — both ignored in v1.
            let inputs = hjkl_engine::decode_macro(&keys);
            for input in inputs {
                hjkl_vim::dispatch_input(app.active_editor_mut(), input);
            }
            // Canonical post-edit sync, NOT settle — see nvim_input (#262).
            app.sync_after_engine_mutation();
            ok(stdout, msgid, Value::Nil)
        }

        "nvim_exec2" => {
            // nvim_exec2(src: string, opts: dict|nil)
            // → always returns a dict; if opts["output"]==true returns {"output": ""}
            //   (hjkl routes command output to its message bus; capturing it is a
            //   future task — v1 always returns an empty string for output).
            //
            // src is split on '\n'; each non-empty trimmed line (with a leading ':'
            // stripped) is dispatched as an ex command.
            let p = match as_array(params) {
                Ok(p) => p,
                Err(e) => return err(stdout, msgid, &e),
            };
            let src = match param_str(p, 0) {
                Ok(s) => s,
                Err(e) => return err(stdout, msgid, &e),
            };
            // Determine whether the caller wants captured output.
            let want_output = match p.get(1) {
                Some(Value::Map(m)) => map_get(m, "output")
                    .and_then(|v| {
                        if let Value::Boolean(b) = v {
                            Some(*b)
                        } else {
                            None
                        }
                    })
                    .unwrap_or(false),
                _ => false,
            };
            // Execute each line.
            for line in src.split('\n') {
                let line = line.trim().strip_prefix(':').unwrap_or(line.trim());
                if !line.is_empty() {
                    app.dispatch_ex(line);
                }
            }
            if app.exit_requested {
                *should_quit = true;
            }
            settle(app);
            // nvim_exec2 always returns a Map dict.
            let result_map = if want_output {
                // v1: output capture not implemented — always returns empty string.
                Value::Map(vec![(Value::from("output"), Value::from(""))])
            } else {
                Value::Map(vec![])
            };
            ok(stdout, msgid, result_map)
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
    // Stdout is the rpc channel here — keep the clipboard off so an OSC 52
    // fallback can't write escapes into the protocol stream (#264).
    crate::host::disable_clipboard_for_rpc();
    let mut app = build_app(files.into_iter().next())?;

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut stdin_lock = stdin.lock();
    let mut stdout_lock = stdout.lock();

    // We need a buffered reader to pull bytes as they arrive.
    let mut reader = std::io::BufReader::new(&mut stdin_lock);

    run_with_io(&mut app, &mut reader, &mut stdout_lock).map(|()| 0)
}

/// Body of [`run`], parameterized over the transport so it's testable
/// in-process without real stdin/stdout — `run` is a thin wrapper that
/// supplies the real stdio handles.
///
/// Runs [`run_loop`] to completion, THEN tears down `app` on every exit
/// path — client closed stdin, a malformed-stream `?`-propagated error, or
/// a `should_quit` request, not just the "loop broke cleanly" case. Mirrors
/// the TUI's teardown pair (main.rs's `app.shutdown()` + `App::run`'s
/// `cleanup_swaps_on_exit()`): without this, an attached LSP server
/// (rust-analyzer, gopls, …) is orphaned — the exact bug fixed for the TUI
/// in b00cbf11 — and stale swap files trigger false recovery prompts on the
/// next launch (audit R2, fix 2).
fn run_with_io<R: std::io::Read>(
    app: &mut crate::app::App,
    reader: &mut R,
    stdout: &mut impl Write,
) -> Result<()> {
    let result = run_loop(app, reader, stdout);
    app.cleanup_swaps_on_exit();
    app.shutdown();
    result
}

/// The msgpack-rpc read/dispatch loop, broken out of [`run_with_io`] so
/// teardown (`cleanup_swaps_on_exit` + `shutdown`) runs on every exit path —
/// including an `Err` propagated via `?` from `dispatch`, which previously
/// skipped `run`'s post-loop cleanup entirely because it returned out of
/// `run` itself.
fn run_loop<R: std::io::Read>(
    app: &mut crate::app::App,
    reader: &mut R,
    stdout: &mut impl Write,
) -> Result<()> {
    let mut should_quit = false;
    loop {
        // Read one msgpack value. Returns Err on EOF or protocol error.
        let msg = match rmpv::decode::read_value(reader) {
            Ok(v) => v,
            Err(e) => {
                use rmpv::decode::Error;
                match e {
                    // ANY failure to read bytes off stdin — EOF, broken pipe,
                    // connection reset, closed handle — means the peer is gone.
                    // Exit cleanly. Previously only `UnexpectedEof` broke and
                    // every other io kind `continue`d, so a pipe-close that
                    // returned a different kind (observed on the ubuntu CI
                    // runner) span-looped at 100% CPU forever — hanging the
                    // parent's `child.wait()` and starving sibling tests (#264).
                    Error::InvalidMarkerRead(_) | Error::InvalidDataRead(_) => break,
                    _ => {
                        // Structurally malformed value on a still-live stream
                        // (bytes were consumed) — log and skip to the next.
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
                        let _ = err(stdout, msgid, "missing method");
                        continue;
                    }
                };
                let params = arr.get(3).cloned().unwrap_or(Value::Array(vec![]));
                dispatch(app, &mut should_quit, &method, &params, stdout, msgid)?;
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
                dispatch(app, &mut should_quit, &method, &params, &mut dev_null, 0)?;
                if should_quit {
                    break;
                }
            }
            _ => {
                eprintln!("hjkl --nvim-api: unexpected message type {msg_type}");
            }
        }
    }

    Ok(())
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use hjkl_vim::VimEditorExt;
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

    /// Encode a window handle `Value::Ext(WINDOW_EXT, encode_id(id))` suitable
    /// for passing as a param to dispatch.
    fn make_win_param(id: u64) -> Value {
        win_handle(id)
    }

    // ── window tests ──────────────────────────────────────────────────────

    #[test]
    fn test_nvim_list_wins_grows_after_vsplit() {
        let mut app = build_app(None).unwrap();

        // Before split: exactly 1 window.
        let before = {
            let resp = call(&mut app, "nvim_list_wins", vec![]);
            match assert_ok(resp) {
                Value::Array(v) => v.len(),
                other => panic!("expected array, got {other:?}"),
            }
        };
        assert_eq!(before, 1);

        // Create a second window via vsplit.
        {
            let resp = call(&mut app, "nvim_command", vec![Value::from("vsplit")]);
            assert_ok(resp);
        }

        let after = {
            let resp = call(&mut app, "nvim_list_wins", vec![]);
            match assert_ok(resp) {
                Value::Array(v) => v.len(),
                other => panic!("expected array, got {other:?}"),
            }
        };
        assert_eq!(after, before + 1, "list_wins should grow by 1 after vsplit");
    }

    #[test]
    fn test_nvim_get_current_win_in_list() {
        let mut app = build_app(None).unwrap();

        // Create a second window.
        {
            let resp = call(&mut app, "nvim_command", vec![Value::from("vsplit")]);
            assert_ok(resp);
        }

        let cur_win = {
            let resp = call(&mut app, "nvim_get_current_win", vec![]);
            assert_ok(resp)
        };

        let wins = {
            let resp = call(&mut app, "nvim_list_wins", vec![]);
            match assert_ok(resp) {
                Value::Array(v) => v,
                other => panic!("expected array, got {other:?}"),
            }
        };

        assert_eq!(wins.len(), 2);
        assert!(
            wins.contains(&cur_win),
            "current window should be in the list"
        );
    }

    #[test]
    fn test_nvim_set_current_win_switches_focus() {
        let mut app = build_app(None).unwrap();

        // Create a second window.
        {
            let resp = call(&mut app, "nvim_command", vec![Value::from("vsplit")]);
            assert_ok(resp);
        }

        let wins = {
            let resp = call(&mut app, "nvim_list_wins", vec![]);
            match assert_ok(resp) {
                Value::Array(v) => v,
                other => panic!("expected array, got {other:?}"),
            }
        };
        assert_eq!(wins.len(), 2);

        // Find the window that is NOT focused.
        let cur_win = {
            let resp = call(&mut app, "nvim_get_current_win", vec![]);
            assert_ok(resp)
        };
        let other_win = wins.iter().find(|w| **w != cur_win).unwrap().clone();

        // Switch to the other window.
        {
            let resp = call(&mut app, "nvim_set_current_win", vec![other_win.clone()]);
            assert_ok(resp);
        }

        // Current win should now be the other one.
        let new_cur = {
            let resp = call(&mut app, "nvim_get_current_win", vec![]);
            assert_ok(resp)
        };
        assert_eq!(
            new_cur, other_win,
            "focus should have moved to the other window"
        );
        assert_ne!(new_cur, cur_win);
    }

    #[test]
    fn test_nvim_win_get_buf_returns_handle() {
        let mut app = build_app(None).unwrap();

        // Create a second window.
        {
            let resp = call(&mut app, "nvim_command", vec![Value::from("vsplit")]);
            assert_ok(resp);
        }

        // Get the current window and its buffer.
        let cur_win = {
            let resp = call(&mut app, "nvim_get_current_win", vec![]);
            assert_ok(resp)
        };

        let buf_from_win = {
            let resp = call(&mut app, "nvim_win_get_buf", vec![cur_win]);
            assert_ok(resp)
        };

        let cur_buf = {
            let resp = call(&mut app, "nvim_get_current_buf", vec![]);
            assert_ok(resp)
        };

        assert_eq!(
            buf_from_win, cur_buf,
            "nvim_win_get_buf for focused win should equal nvim_get_current_buf"
        );
    }

    #[test]
    fn test_nvim_win_set_buf_redirects_window() {
        let mut app = build_app(None).unwrap();

        // Create a new scratch buffer.
        let new_buf = {
            let resp = call(
                &mut app,
                "nvim_create_buf",
                vec![Value::Boolean(true), Value::Boolean(false)],
            );
            assert_ok(resp)
        };

        // Create a second window.
        {
            let resp = call(&mut app, "nvim_command", vec![Value::from("vsplit")]);
            assert_ok(resp);
        }

        let wins = {
            let resp = call(&mut app, "nvim_list_wins", vec![]);
            match assert_ok(resp) {
                Value::Array(v) => v,
                other => panic!("expected array, got {other:?}"),
            }
        };

        // Pick the non-focused window.
        let cur_win = {
            let resp = call(&mut app, "nvim_get_current_win", vec![]);
            assert_ok(resp)
        };
        let other_win = wins.iter().find(|w| **w != cur_win).unwrap().clone();

        // Point the other window at the new buffer.
        {
            let resp = call(
                &mut app,
                "nvim_win_set_buf",
                vec![other_win.clone(), new_buf.clone()],
            );
            assert_ok(resp);
        }

        // Verify: nvim_win_get_buf for that window should return new_buf.
        let win_buf = {
            let resp = call(&mut app, "nvim_win_get_buf", vec![other_win]);
            assert_ok(resp)
        };
        assert_eq!(
            win_buf, new_buf,
            "window should now point at the new buffer"
        );
    }

    #[test]
    fn test_nvim_win_cursor_roundtrip_for_specific_window() {
        let mut app = build_app(None).unwrap();

        // Seed the buffer with some lines so we can set a non-trivial cursor.
        {
            let buf_handle = {
                let resp = call(&mut app, "nvim_get_current_buf", vec![]);
                assert_ok(resp)
            };
            let resp = call(
                &mut app,
                "nvim_buf_set_lines",
                vec![
                    buf_handle,
                    Value::from(0i64),
                    Value::from(-1i64),
                    Value::Boolean(false),
                    Value::Array(vec![
                        Value::from("first line"),
                        Value::from("second line"),
                        Value::from("third line"),
                    ]),
                ],
            );
            assert_ok(resp);
        }

        // Create a second window.
        {
            let resp = call(&mut app, "nvim_command", vec![Value::from("vsplit")]);
            assert_ok(resp);
        }

        let cur_win = {
            let resp = call(&mut app, "nvim_get_current_win", vec![]);
            assert_ok(resp)
        };

        // Set cursor to row 2, col 3 (1-based row, byte-col).
        {
            let resp = call(
                &mut app,
                "nvim_win_set_cursor",
                vec![
                    cur_win.clone(),
                    Value::Array(vec![Value::from(2i64), Value::from(3i64)]),
                ],
            );
            assert_ok(resp);
        }

        // Get cursor back and verify.
        let cursor = {
            let resp = call(&mut app, "nvim_win_get_cursor", vec![cur_win]);
            match assert_ok(resp) {
                Value::Array(v) => v,
                other => panic!("expected array, got {other:?}"),
            }
        };
        assert_eq!(
            cursor[0],
            Value::from(2i64),
            "cursor row should be 2 (1-based)"
        );
        assert_eq!(
            cursor[1],
            Value::from(3i64),
            "cursor col should be 3 (byte-col)"
        );
    }

    #[test]
    fn test_make_win_param_is_window_ext() {
        let handle = make_win_param(42);
        match &handle {
            Value::Ext(tag, bytes) => {
                assert_eq!(*tag, WINDOW_EXT);
                let mut cur = std::io::Cursor::new(bytes.as_slice());
                let inner = rmpv::decode::read_value(&mut cur).unwrap();
                assert_eq!(inner.as_u64(), Some(42));
            }
            other => panic!("expected Ext window handle, got {other:?}"),
        }
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

    // ── setqflist / getqflist roundtrip ───────────────────────────────────────

    #[test]
    fn test_setqflist_getqflist_roundtrip() {
        let mut app = build_app(None).unwrap();

        // Build two qf dicts.
        let entry1 = Value::Map(vec![
            (Value::from("filename"), Value::from("/tmp/a.rs")),
            (Value::from("lnum"), Value::from(3i64)),
            (Value::from("col"), Value::from(7i64)),
            (Value::from("text"), Value::from("error one")),
            (Value::from("type"), Value::from("E")),
        ]);
        let entry2 = Value::Map(vec![
            (Value::from("filename"), Value::from("/tmp/b.rs")),
            (Value::from("lnum"), Value::from(10i64)),
            (Value::from("col"), Value::from(1i64)),
            (Value::from("text"), Value::from("warning two")),
            (Value::from("type"), Value::from("W")),
        ]);

        // setqflist([entry1, entry2])
        {
            let resp = call(
                &mut app,
                "nvim_call_function",
                vec![
                    Value::from("setqflist"),
                    Value::Array(vec![Value::Array(vec![entry1, entry2])]),
                ],
            );
            let r = assert_ok(resp);
            assert_eq!(r, Value::from(0i64), "setqflist should return 0");
        }

        // getqflist() should return 2 entries with correct 1-based lnum/col.
        let entries = {
            let resp = call(
                &mut app,
                "nvim_call_function",
                vec![Value::from("getqflist"), Value::Array(vec![])],
            );
            match assert_ok(resp) {
                Value::Array(v) => v,
                other => panic!("expected array, got {other:?}"),
            }
        };

        assert_eq!(entries.len(), 2, "should have 2 entries");

        // Check entry 1: lnum=3, col=7, text="error one"
        let e1 = match &entries[0] {
            Value::Map(m) => m.clone(),
            other => panic!("expected map, got {other:?}"),
        };
        let get = |m: &[(Value, Value)], k: &str| -> Value {
            m.iter()
                .find_map(|(key, v)| {
                    if let Value::String(s) = key
                        && s.as_str() == Some(k)
                    {
                        return Some(v.clone());
                    }
                    None
                })
                .unwrap_or(Value::Nil)
        };
        assert_eq!(get(&e1, "lnum"), Value::from(3i64));
        assert_eq!(get(&e1, "col"), Value::from(7i64));
        assert_eq!(get(&e1, "text"), Value::from("error one"));
        assert_eq!(get(&e1, "valid"), Value::from(1i64));

        // Check entry 2: lnum=10, col=1
        let e2 = match &entries[1] {
            Value::Map(m) => m.clone(),
            other => panic!("expected map, got {other:?}"),
        };
        assert_eq!(get(&e2, "lnum"), Value::from(10i64));
        assert_eq!(get(&e2, "col"), Value::from(1i64));
        assert_eq!(get(&e2, "text"), Value::from("warning two"));
    }

    // ── getloclist empty, setloclist roundtrip ────────────────────────────────

    #[test]
    fn test_getloclist_empty_then_setloclist() {
        let mut app = build_app(None).unwrap();

        // Initially empty.
        let initial = {
            let resp = call(
                &mut app,
                "nvim_call_function",
                vec![
                    Value::from("getloclist"),
                    Value::Array(vec![Value::from(0i64)]),
                ],
            );
            match assert_ok(resp) {
                Value::Array(v) => v,
                other => panic!("expected array, got {other:?}"),
            }
        };
        assert!(initial.is_empty(), "loclist should start empty");

        // setloclist(0, [entry])
        let loc_entry = Value::Map(vec![
            (Value::from("filename"), Value::from("/tmp/loc.rs")),
            (Value::from("lnum"), Value::from(5i64)),
            (Value::from("col"), Value::from(2i64)),
            (Value::from("text"), Value::from("loc msg")),
        ]);
        {
            let resp = call(
                &mut app,
                "nvim_call_function",
                vec![
                    Value::from("setloclist"),
                    Value::Array(vec![
                        Value::from(0i64), // window arg (ignored)
                        Value::Array(vec![loc_entry]),
                    ]),
                ],
            );
            let r = assert_ok(resp);
            assert_eq!(r, Value::from(0i64));
        }

        // getloclist should now have 1 entry.
        let after = {
            let resp = call(
                &mut app,
                "nvim_call_function",
                vec![
                    Value::from("getloclist"),
                    Value::Array(vec![Value::from(0i64)]),
                ],
            );
            match assert_ok(resp) {
                Value::Array(v) => v,
                other => panic!("expected array, got {other:?}"),
            }
        };
        assert_eq!(
            after.len(),
            1,
            "loclist should have 1 entry after setloclist"
        );
        if let Value::Map(ref m) = after[0] {
            let get = |m: &[(Value, Value)], k: &str| -> Value {
                m.iter()
                    .find_map(|(key, v)| {
                        if let Value::String(s) = key
                            && s.as_str() == Some(k)
                        {
                            return Some(v.clone());
                        }
                        None
                    })
                    .unwrap_or(Value::Nil)
            };
            assert_eq!(get(m, "lnum"), Value::from(5i64));
            assert_eq!(get(m, "col"), Value::from(2i64));
            assert_eq!(get(m, "text"), Value::from("loc msg"));
        } else {
            panic!("expected map entry");
        }
    }

    // ── bufnr("%") matches nvim_get_current_buf id ────────────────────────────

    #[test]
    fn test_bufnr_percent_matches_current_buf() {
        let mut app = build_app(None).unwrap();

        // Get the current buffer id via nvim_get_current_buf.
        let cur_handle = {
            let resp = call(&mut app, "nvim_get_current_buf", vec![]);
            assert_ok(resp)
        };
        // Decode the ext handle to a u64 id.
        let cur_id = match &cur_handle {
            Value::Ext(_, bytes) => {
                let mut c = std::io::Cursor::new(bytes.as_slice());
                rmpv::decode::read_value(&mut c)
                    .unwrap()
                    .as_u64()
                    .expect("id")
            }
            other => panic!("expected Ext handle, got {other:?}"),
        };

        // bufnr("%") should return the same id.
        let resp = call(
            &mut app,
            "nvim_call_function",
            vec![Value::from("bufnr"), Value::Array(vec![Value::from("%")])],
        );
        let result = assert_ok(resp);
        assert_eq!(
            result,
            Value::from(cur_id as i64),
            "bufnr('%') should match current buffer id"
        );
    }

    // ── expand("%:t") / expand("%:e") roundtrip via nvim_buf_set_name ─────────

    #[test]
    fn test_expand_modifiers_roundtrip() {
        let mut app = build_app(None).unwrap();

        // Set the current buffer name to a known path.
        let cur_handle = {
            let resp = call(&mut app, "nvim_get_current_buf", vec![]);
            assert_ok(resp)
        };
        {
            let resp = call(
                &mut app,
                "nvim_buf_set_name",
                vec![cur_handle, Value::from("/home/user/project/main.rs")],
            );
            assert_ok(resp);
        }

        // expand("%:t") should return "main.rs"
        let tail = {
            let resp = call(
                &mut app,
                "nvim_call_function",
                vec![
                    Value::from("expand"),
                    Value::Array(vec![Value::from("%:t")]),
                ],
            );
            match assert_ok(resp) {
                Value::String(s) => s.as_str().unwrap_or("").to_owned(),
                other => panic!("expected string, got {other:?}"),
            }
        };
        assert_eq!(tail, "main.rs", "expand('%:t') should be the filename tail");

        // expand("%:e") should return "rs"
        let ext = {
            let resp = call(
                &mut app,
                "nvim_call_function",
                vec![
                    Value::from("expand"),
                    Value::Array(vec![Value::from("%:e")]),
                ],
            );
            match assert_ok(resp) {
                Value::String(s) => s.as_str().unwrap_or("").to_owned(),
                other => panic!("expected string, got {other:?}"),
            }
        };
        assert_eq!(ext, "rs", "expand('%:e') should be the extension");
    }

    // ── expand("%:r") strips only the final-component extension ───────────────

    #[test]
    fn test_expand_root_modifier_respects_path_components() {
        let mut app = build_app(None).unwrap();
        let cur_handle = assert_ok(call(&mut app, "nvim_get_current_buf", vec![]));

        let root_of = |app: &mut crate::app::App, path: &str| -> String {
            assert_ok(call(
                app,
                "nvim_buf_set_name",
                vec![cur_handle.clone(), Value::from(path)],
            ));
            match assert_ok(call(
                app,
                "nvim_call_function",
                vec![
                    Value::from("expand"),
                    Value::Array(vec![Value::from("%:r")]),
                ],
            )) {
                Value::String(s) => s.as_str().unwrap_or("").to_owned(),
                other => panic!("expected string, got {other:?}"),
            }
        };

        // Normal extension strip.
        assert_eq!(root_of(&mut app, "/home/user/main.rs"), "/home/user/main");
        // A dot in a *directory* name must NOT be treated as an extension.
        assert_eq!(
            root_of(&mut app, "/home/user/proj.v2/main"),
            "/home/user/proj.v2/main"
        );
        // A dotfile has no extension.
        assert_eq!(
            root_of(&mut app, "/home/user/.bashrc"),
            "/home/user/.bashrc"
        );
    }

    // ── line(".") and col(".") are 1-based ────────────────────────────────────

    #[test]
    fn test_line_col_are_one_based() {
        let mut app = build_app(None).unwrap();

        // Seed content.
        {
            let buf = {
                let resp = call(&mut app, "nvim_get_current_buf", vec![]);
                assert_ok(resp)
            };
            let resp = call(
                &mut app,
                "nvim_buf_set_lines",
                vec![
                    buf,
                    Value::from(0i64),
                    Value::from(-1i64),
                    Value::Boolean(false),
                    Value::Array(vec![
                        Value::from("hello world"),
                        Value::from("second line"),
                        Value::from("third"),
                    ]),
                ],
            );
            assert_ok(resp);
        }

        // Move cursor to row=2 (1-based), col=5 via nvim_win_set_cursor.
        {
            let win = {
                let resp = call(&mut app, "nvim_get_current_win", vec![]);
                assert_ok(resp)
            };
            let resp = call(
                &mut app,
                "nvim_win_set_cursor",
                vec![
                    win,
                    Value::Array(vec![Value::from(2i64), Value::from(4i64)]),
                ],
            );
            assert_ok(resp);
        }

        // line(".") should be 2 (1-based row).
        let line_dot = {
            let resp = call(
                &mut app,
                "nvim_call_function",
                vec![Value::from("line"), Value::Array(vec![Value::from(".")])],
            );
            match assert_ok(resp) {
                Value::Integer(n) => n.as_i64().unwrap(),
                other => panic!("expected integer, got {other:?}"),
            }
        };
        assert_eq!(line_dot, 2, "line('.') should be 2 (1-based)");

        // col(".") should be 5 (char-col 4 + 1).
        let col_dot = {
            let resp = call(
                &mut app,
                "nvim_call_function",
                vec![Value::from("col"), Value::Array(vec![Value::from(".")])],
            );
            match assert_ok(resp) {
                Value::Integer(n) => n.as_i64().unwrap(),
                other => panic!("expected integer, got {other:?}"),
            }
        };
        assert_eq!(col_dot, 5, "col('.') should be 5 (1-based char col)");

        // line("$") should be 3 (total lines).
        let line_dollar = {
            let resp = call(
                &mut app,
                "nvim_call_function",
                vec![Value::from("line"), Value::Array(vec![Value::from("$")])],
            );
            match assert_ok(resp) {
                Value::Integer(n) => n.as_i64().unwrap(),
                other => panic!("expected integer, got {other:?}"),
            }
        };
        assert_eq!(line_dollar, 3, "line('$') should be 3 (total line count)");
    }

    // ── nvim_win_get_height / get_width / set_height / set_width ─────────

    #[test]
    fn test_nvim_win_get_height_single_window() {
        // A single window in an 80×24 headless terminal should report height 23
        // (24 total minus 1 status-line row, matching neovim convention).
        let mut app = build_app(None).unwrap();
        let win = {
            let resp = call(&mut app, "nvim_get_current_win", vec![]);
            assert_ok(resp)
        };
        let h = {
            let resp = call(&mut app, "nvim_win_get_height", vec![win]);
            match assert_ok(resp) {
                Value::Integer(n) => n.as_i64().unwrap(),
                other => panic!("expected integer, got {other:?}"),
            }
        };
        assert_eq!(
            h, 23,
            "single window height should be 23 (24 - 1 status row)"
        );
    }

    #[test]
    fn test_nvim_win_get_width_single_window() {
        // A single window should report the full 80 columns.
        let mut app = build_app(None).unwrap();
        let win = {
            let resp = call(&mut app, "nvim_get_current_win", vec![]);
            assert_ok(resp)
        };
        let w = {
            let resp = call(&mut app, "nvim_win_get_width", vec![win]);
            match assert_ok(resp) {
                Value::Integer(n) => n.as_i64().unwrap(),
                other => panic!("expected integer, got {other:?}"),
            }
        };
        assert_eq!(w, 80, "single window width should be 80");
    }

    #[test]
    fn test_nvim_win_get_width_after_vsplit_sums_to_total() {
        // After vsplit: two windows. Their widths + 1 separator should equal 80.
        let mut app = build_app(None).unwrap();
        {
            let resp = call(&mut app, "nvim_command", vec![Value::from("vsplit")]);
            assert_ok(resp);
        }
        let wins = {
            let resp = call(&mut app, "nvim_list_wins", vec![]);
            match assert_ok(resp) {
                Value::Array(v) => v,
                other => panic!("expected array, got {other:?}"),
            }
        };
        assert_eq!(wins.len(), 2);

        let w0 = {
            let resp = call(&mut app, "nvim_win_get_width", vec![wins[0].clone()]);
            match assert_ok(resp) {
                Value::Integer(n) => n.as_i64().unwrap(),
                other => panic!("expected integer, got {other:?}"),
            }
        };
        let w1 = {
            let resp = call(&mut app, "nvim_win_get_width", vec![wins[1].clone()]);
            match assert_ok(resp) {
                Value::Integer(n) => n.as_i64().unwrap(),
                other => panic!("expected integer, got {other:?}"),
            }
        };
        // w0 + 1 (separator) + w1 == 80
        assert_eq!(
            w0 + 1 + w1,
            80,
            "window widths + separator must sum to 80, got {w0} + 1 + {w1}"
        );
        // Both should be approximately half.
        assert!(
            (30..=50).contains(&w0),
            "left width should be near half, got {w0}"
        );
        assert!(
            (30..=50).contains(&w1),
            "right width should be near half, got {w1}"
        );
    }

    #[test]
    fn test_nvim_win_get_height_after_vsplit_unchanged() {
        // After vsplit: both windows should still have height 23.
        let mut app = build_app(None).unwrap();
        {
            let resp = call(&mut app, "nvim_command", vec![Value::from("vsplit")]);
            assert_ok(resp);
        }
        let wins = {
            let resp = call(&mut app, "nvim_list_wins", vec![]);
            match assert_ok(resp) {
                Value::Array(v) => v,
                other => panic!("expected array, got {other:?}"),
            }
        };
        for win in &wins {
            let h = {
                let resp = call(&mut app, "nvim_win_get_height", vec![win.clone()]);
                match assert_ok(resp) {
                    Value::Integer(n) => n.as_i64().unwrap(),
                    other => panic!("expected integer, got {other:?}"),
                }
            };
            assert_eq!(h, 23, "after vsplit, height should remain 23, got {h}");
        }
    }

    #[test]
    fn test_nvim_win_get_height_after_split_sums_to_total() {
        // After :split (horizontal): heights + 1 separator == 23.
        let mut app = build_app(None).unwrap();
        {
            let resp = call(&mut app, "nvim_command", vec![Value::from("split")]);
            assert_ok(resp);
        }
        let wins = {
            let resp = call(&mut app, "nvim_list_wins", vec![]);
            match assert_ok(resp) {
                Value::Array(v) => v,
                other => panic!("expected array, got {other:?}"),
            }
        };
        assert_eq!(wins.len(), 2);
        let h0 = {
            let resp = call(&mut app, "nvim_win_get_height", vec![wins[0].clone()]);
            match assert_ok(resp) {
                Value::Integer(n) => n.as_i64().unwrap(),
                other => panic!("expected integer, got {other:?}"),
            }
        };
        let h1 = {
            let resp = call(&mut app, "nvim_win_get_height", vec![wins[1].clone()]);
            match assert_ok(resp) {
                Value::Integer(n) => n.as_i64().unwrap(),
                other => panic!("expected integer, got {other:?}"),
            }
        };
        assert_eq!(
            h0 + 1 + h1,
            23,
            "window heights + separator must sum to 23, got {h0} + 1 + {h1}"
        );
    }

    #[test]
    fn test_nvim_win_set_width_moves_split_toward_target() {
        // After vsplit: request that the focused window become 30 cols wide.
        // The resulting width should be closer to 30 than the original ~40.
        let mut app = build_app(None).unwrap();
        {
            let resp = call(&mut app, "nvim_command", vec![Value::from("vsplit")]);
            assert_ok(resp);
        }
        let cur_win = {
            let resp = call(&mut app, "nvim_get_current_win", vec![]);
            assert_ok(resp)
        };

        // Current width before set.
        let w_before = {
            let resp = call(&mut app, "nvim_win_get_width", vec![cur_win.clone()]);
            match assert_ok(resp) {
                Value::Integer(n) => n.as_i64().unwrap(),
                other => panic!("expected integer, got {other:?}"),
            }
        };

        // Set width to 30.
        {
            let resp = call(
                &mut app,
                "nvim_win_set_width",
                vec![cur_win.clone(), Value::from(30i64)],
            );
            assert_ok(resp);
        }

        let w_after = {
            let resp = call(&mut app, "nvim_win_get_width", vec![cur_win]);
            match assert_ok(resp) {
                Value::Integer(n) => n.as_i64().unwrap(),
                other => panic!("expected integer, got {other:?}"),
            }
        };

        // After set_width(30): width should have moved toward 30 (or stay 30).
        // The ratio math gives us ratio = 30/80 = 0.375; headless rect gives
        // a_w = round(80 * 0.375) = 30, minus separator = 29.
        // Either way, it must be less than the original ~40.
        assert!(
            (w_after as i64 - 30).abs() <= (w_before as i64 - 30).abs(),
            "width should move toward 30: before={w_before}, after={w_after}"
        );
    }

    #[test]
    fn test_nvim_win_set_height_single_window_noop() {
        // Single window has no enclosing horizontal split → set_height is a no-op.
        let mut app = build_app(None).unwrap();
        let win = {
            let resp = call(&mut app, "nvim_get_current_win", vec![]);
            assert_ok(resp)
        };
        // set_height on a single window should return Ok(Nil) without error.
        let resp = call(
            &mut app,
            "nvim_win_set_height",
            vec![win, Value::from(10i64)],
        );
        assert_ok(resp);
    }

    #[test]
    fn test_nvim_win_get_height_invalid_window_returns_err() {
        let mut app = build_app(None).unwrap();
        let resp = call(&mut app, "nvim_win_get_height", vec![make_win_param(9999)]);
        assert!(
            resp[2] != Value::Nil,
            "invalid window id should return an error"
        );
    }

    #[test]
    fn test_nvim_win_get_width_invalid_window_returns_err() {
        let mut app = build_app(None).unwrap();
        let resp = call(&mut app, "nvim_win_get_width", vec![make_win_param(9999)]);
        assert!(
            resp[2] != Value::Nil,
            "invalid window id should return an error"
        );
    }

    // ── tabpage helpers ───────────────────────────────────────────────────

    fn make_tab_param(id: u64) -> Value {
        tab_handle(id)
    }

    fn decode_tab_id(v: &Value) -> u64 {
        match v {
            Value::Ext(tag, bytes) => {
                assert_eq!(*tag, TABPAGE_EXT);
                let mut c = std::io::Cursor::new(bytes.as_slice());
                rmpv::decode::read_value(&mut c)
                    .unwrap()
                    .as_u64()
                    .expect("tab id")
            }
            other => panic!("expected tabpage Ext handle, got {other:?}"),
        }
    }

    // ── tabpage tests ─────────────────────────────────────────────────────

    #[test]
    fn test_nvim_list_tabpages_starts_with_one() {
        let mut app = build_app(None).unwrap();
        let resp = call(&mut app, "nvim_list_tabpages", vec![]);
        let tabs = match assert_ok(resp) {
            Value::Array(v) => v,
            other => panic!("expected array, got {other:?}"),
        };
        assert_eq!(tabs.len(), 1, "fresh app should have 1 tab");
        // Entries must be TABPAGE_EXT handles.
        for t in &tabs {
            decode_tab_id(t);
        }
    }

    #[test]
    fn test_nvim_list_tabpages_grows_after_tabnew() {
        let mut app = build_app(None).unwrap();

        // Before tabnew.
        let before = {
            let resp = call(&mut app, "nvim_list_tabpages", vec![]);
            match assert_ok(resp) {
                Value::Array(v) => v.len(),
                other => panic!("expected array, got {other:?}"),
            }
        };

        // Open a second tab.
        {
            let resp = call(&mut app, "nvim_command", vec![Value::from("tabnew")]);
            assert_ok(resp);
        }

        let after = {
            let resp = call(&mut app, "nvim_list_tabpages", vec![]);
            match assert_ok(resp) {
                Value::Array(v) => v.len(),
                other => panic!("expected array, got {other:?}"),
            }
        };
        assert_eq!(
            after,
            before + 1,
            "list_tabpages should grow by 1 after tabnew"
        );
        assert_eq!(after, 2, "should have exactly 2 tabs");
    }

    #[test]
    fn test_nvim_get_current_tabpage_and_set_switches() {
        let mut app = build_app(None).unwrap();

        // Open a second tab so we have two.
        {
            let resp = call(&mut app, "nvim_command", vec![Value::from("tabnew")]);
            assert_ok(resp);
        }

        // We should now be on tab 1 (0-indexed).
        let cur = {
            let resp = call(&mut app, "nvim_get_current_tabpage", vec![]);
            assert_ok(resp)
        };
        let cur_id = decode_tab_id(&cur);
        assert_eq!(cur_id, 1, "after tabnew the active tab index should be 1");

        // Switch back to tab 0.
        {
            let resp = call(
                &mut app,
                "nvim_set_current_tabpage",
                vec![make_tab_param(0)],
            );
            assert_ok(resp);
        }

        let new_cur = {
            let resp = call(&mut app, "nvim_get_current_tabpage", vec![]);
            assert_ok(resp)
        };
        assert_eq!(decode_tab_id(&new_cur), 0, "should be back on tab 0");
    }

    #[test]
    fn test_nvim_set_current_tabpage_invalid_returns_err() {
        let mut app = build_app(None).unwrap();
        // Only 1 tab; index 99 is invalid.
        let resp = call(
            &mut app,
            "nvim_set_current_tabpage",
            vec![make_tab_param(99)],
        );
        assert!(
            resp[2] != Value::Nil,
            "invalid tabpage should return an error"
        );
    }

    #[test]
    fn test_nvim_tabpage_list_wins_returns_at_least_one() {
        let mut app = build_app(None).unwrap();

        // Tab 0 — the initial tab — should have exactly one window.
        let wins = {
            let resp = call(&mut app, "nvim_tabpage_list_wins", vec![make_tab_param(0)]);
            match assert_ok(resp) {
                Value::Array(v) => v,
                other => panic!("expected array, got {other:?}"),
            }
        };
        assert!(!wins.is_empty(), "tab 0 must have at least 1 window");
        // Each entry must be a WINDOW_EXT handle.
        for w in &wins {
            match w {
                Value::Ext(tag, _) => assert_eq!(*tag, WINDOW_EXT),
                other => panic!("expected window Ext handle, got {other:?}"),
            }
        }
    }

    #[test]
    fn test_nvim_tabpage_is_valid() {
        let mut app = build_app(None).unwrap();

        let valid = {
            let resp = call(&mut app, "nvim_tabpage_is_valid", vec![make_tab_param(0)]);
            match assert_ok(resp) {
                Value::Boolean(b) => b,
                other => panic!("expected boolean, got {other:?}"),
            }
        };
        assert!(valid, "tab 0 should be valid");

        let invalid = {
            let resp = call(&mut app, "nvim_tabpage_is_valid", vec![make_tab_param(99)]);
            match assert_ok(resp) {
                Value::Boolean(b) => b,
                other => panic!("expected boolean, got {other:?}"),
            }
        };
        assert!(!invalid, "tab 99 should be invalid");
    }

    // ── nvim_buf_line_count tests ─────────────────────────────────────────

    #[test]
    fn test_nvim_buf_line_count_matches_get_lines_len() {
        let mut app = build_app(None).unwrap();

        // Seed multi-line content.
        let buf = {
            let resp = call(&mut app, "nvim_get_current_buf", vec![]);
            assert_ok(resp)
        };
        {
            let resp = call(
                &mut app,
                "nvim_buf_set_lines",
                vec![
                    buf.clone(),
                    Value::from(0i64),
                    Value::from(-1i64),
                    Value::Boolean(false),
                    Value::Array(vec![
                        Value::from("alpha"),
                        Value::from("beta"),
                        Value::from("gamma"),
                    ]),
                ],
            );
            assert_ok(resp);
        }

        // line_count via new method.
        let count = {
            let resp = call(&mut app, "nvim_buf_line_count", vec![buf.clone()]);
            match assert_ok(resp) {
                Value::Integer(n) => n.as_i64().unwrap(),
                other => panic!("expected integer, got {other:?}"),
            }
        };

        // Line count via get_lines(0,-1).len().
        let get_lines_len = {
            let resp = call(
                &mut app,
                "nvim_buf_get_lines",
                vec![
                    buf,
                    Value::from(0i64),
                    Value::from(-1i64),
                    Value::Boolean(false),
                ],
            );
            match assert_ok(resp) {
                Value::Array(v) => v.len() as i64,
                other => panic!("expected array, got {other:?}"),
            }
        };

        assert_eq!(
            count, get_lines_len,
            "nvim_buf_line_count must equal nvim_buf_get_lines(0,-1).len()"
        );
        assert_eq!(count, 3, "should be 3 lines");
    }

    #[test]
    fn test_nvim_buf_line_count_empty_buffer_is_one() {
        let mut app = build_app(None).unwrap();
        let buf = {
            let resp = call(&mut app, "nvim_get_current_buf", vec![]);
            assert_ok(resp)
        };
        let count = {
            let resp = call(&mut app, "nvim_buf_line_count", vec![buf]);
            match assert_ok(resp) {
                Value::Integer(n) => n.as_i64().unwrap(),
                other => panic!("expected integer, got {other:?}"),
            }
        };
        assert_eq!(count, 1, "empty buffer has 1 line (nvim compat)");
    }

    // ── nvim_get_current_line / nvim_set_current_line ─────────────────────

    #[test]
    fn test_nvim_get_set_current_line_roundtrip() {
        let mut app = build_app(None).unwrap();

        // Seed content.
        let buf = {
            let resp = call(&mut app, "nvim_get_current_buf", vec![]);
            assert_ok(resp)
        };
        {
            let resp = call(
                &mut app,
                "nvim_buf_set_lines",
                vec![
                    buf,
                    Value::from(0i64),
                    Value::from(-1i64),
                    Value::Boolean(false),
                    Value::Array(vec![
                        Value::from("line one"),
                        Value::from("line two"),
                        Value::from("line three"),
                    ]),
                ],
            );
            assert_ok(resp);
        }

        // Move cursor to row 2 (1-based) via nvim_win_set_cursor.
        let win = {
            let resp = call(&mut app, "nvim_get_current_win", vec![]);
            assert_ok(resp)
        };
        {
            let resp = call(
                &mut app,
                "nvim_win_set_cursor",
                vec![
                    win,
                    Value::Array(vec![Value::from(2i64), Value::from(0i64)]),
                ],
            );
            assert_ok(resp);
        }

        // get_current_line should be "line two" (0-based row 1).
        let line = {
            let resp = call(&mut app, "nvim_get_current_line", vec![]);
            match assert_ok(resp) {
                Value::String(s) => s.as_str().unwrap_or("").to_owned(),
                other => panic!("expected string, got {other:?}"),
            }
        };
        assert_eq!(line, "line two");

        // Replace the current line.
        {
            let resp = call(
                &mut app,
                "nvim_set_current_line",
                vec![Value::from("REPLACED")],
            );
            assert_ok(resp);
        }

        // Read it back.
        let after = {
            let resp = call(&mut app, "nvim_get_current_line", vec![]);
            match assert_ok(resp) {
                Value::String(s) => s.as_str().unwrap_or("").to_owned(),
                other => panic!("expected string, got {other:?}"),
            }
        };
        assert_eq!(after, "REPLACED");

        // Other lines must be untouched.
        let buf2 = {
            let resp = call(&mut app, "nvim_get_current_buf", vec![]);
            assert_ok(resp)
        };
        let all_lines = {
            let resp = call(
                &mut app,
                "nvim_buf_get_lines",
                vec![
                    buf2,
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
        assert_eq!(all_lines[0], Value::from("line one"));
        assert_eq!(all_lines[1], Value::from("REPLACED"));
        assert_eq!(all_lines[2], Value::from("line three"));
    }

    // ── nvim_buf_get_text / nvim_buf_set_text ─────────────────────────────

    #[test]
    fn test_nvim_buf_get_text_sub_range() {
        let mut app = build_app(None).unwrap();

        let buf = {
            let resp = call(&mut app, "nvim_get_current_buf", vec![]);
            assert_ok(resp)
        };
        {
            let resp = call(
                &mut app,
                "nvim_buf_set_lines",
                vec![
                    buf.clone(),
                    Value::from(0i64),
                    Value::from(-1i64),
                    Value::Boolean(false),
                    Value::Array(vec![
                        Value::from("hello world"),
                        Value::from("rust lang"),
                        Value::from("done"),
                    ]),
                ],
            );
            assert_ok(resp);
        }

        // get_text: row 0 col 6 → row 0 col 11 = "world"
        let text = {
            let resp = call(
                &mut app,
                "nvim_buf_get_text",
                vec![
                    buf.clone(),
                    Value::from(0i64),  // start_row
                    Value::from(6i64),  // start_col (byte)
                    Value::from(0i64),  // end_row
                    Value::from(11i64), // end_col (byte)
                    Value::Map(vec![]),
                ],
            );
            match assert_ok(resp) {
                Value::Array(v) => v,
                other => panic!("expected array, got {other:?}"),
            }
        };
        assert_eq!(text.len(), 1);
        assert_eq!(text[0], Value::from("world"));

        // Multi-row: row 0 col 6 → row 1 col 4 = ["world", "rust"]
        let multi = {
            let resp = call(
                &mut app,
                "nvim_buf_get_text",
                vec![
                    buf,
                    Value::from(0i64), // start_row
                    Value::from(6i64), // start_col
                    Value::from(1i64), // end_row
                    Value::from(4i64), // end_col
                    Value::Map(vec![]),
                ],
            );
            match assert_ok(resp) {
                Value::Array(v) => v,
                other => panic!("expected array, got {other:?}"),
            }
        };
        assert_eq!(multi.len(), 2);
        assert_eq!(multi[0], Value::from("world"));
        assert_eq!(multi[1], Value::from("rust"));
    }

    #[test]
    fn test_nvim_buf_set_text_splices_correctly() {
        let mut app = build_app(None).unwrap();

        let buf = {
            let resp = call(&mut app, "nvim_get_current_buf", vec![]);
            assert_ok(resp)
        };
        {
            let resp = call(
                &mut app,
                "nvim_buf_set_lines",
                vec![
                    buf.clone(),
                    Value::from(0i64),
                    Value::from(-1i64),
                    Value::Boolean(false),
                    Value::Array(vec![Value::from("hello world"), Value::from("rust lang")]),
                ],
            );
            assert_ok(resp);
        }

        // Replace "world" (row 0, bytes 6..11) with "there"
        {
            let resp = call(
                &mut app,
                "nvim_buf_set_text",
                vec![
                    buf.clone(),
                    Value::from(0i64),  // start_row
                    Value::from(6i64),  // start_col
                    Value::from(0i64),  // end_row
                    Value::from(11i64), // end_col
                    Value::Array(vec![Value::from("there")]),
                ],
            );
            assert_ok(resp);
        }

        // Verify via get_lines.
        let lines = {
            let resp = call(
                &mut app,
                "nvim_buf_get_lines",
                vec![
                    buf,
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
        assert_eq!(lines[0], Value::from("hello there"));
        assert_eq!(lines[1], Value::from("rust lang"));
    }

    // ── nvim_buf_set_lines / nvim_buf_set_text sync chain (audit R2, fix 1) ───

    /// Regression (audit R2, fix 1): `nvim_buf_set_lines` mutates via
    /// `Editor::set_content()` + the nvim-api `settle()` helper, which only
    /// reconciles window editors and flushes a pending recompute — it never
    /// drains `take_content_edits`/`take_content_reset`. Before the fix the
    /// LSP never got a `textDocument/didChange` and the content-reset flag
    /// was left dangling for some unrelated later call to observe instead.
    #[test]
    fn test_nvim_buf_set_lines_syncs_lsp_and_drains_content_reset() {
        let mut app = build_app(None).unwrap();
        app.lsp = Some(hjkl_lsp::LspManager::spawn(hjkl_lsp::LspConfig::default()));

        let buf = {
            let resp = call(&mut app, "nvim_get_current_buf", vec![]);
            assert_ok(resp)
        };
        let resp = call(
            &mut app,
            "nvim_buf_set_lines",
            vec![
                buf,
                Value::from(0i64),
                Value::from(-1i64),
                Value::Boolean(false),
                Value::Array(vec![Value::from("hello")]),
            ],
        );
        assert_ok(resp);

        let dg = app.active_editor().buffer().dirty_gen();
        assert_eq!(
            app.active().last_lsp_dirty_gen,
            Some(dg),
            "nvim_buf_set_lines must notify the LSP (didChange) — settle() alone \
             never drains the content-reset queue set_content() fills"
        );
        assert!(
            !app.active_editor_mut().take_content_reset(),
            "the content-reset flag must already be drained by the sync helper, \
             not left pending for whatever call happens to touch this editor next"
        );

        if let Some(mgr) = app.lsp.take() {
            mgr.shutdown();
        }
    }

    /// Same regression as above, but for `nvim_buf_set_text`.
    #[test]
    fn test_nvim_buf_set_text_syncs_lsp_and_drains_content_reset() {
        let mut app = build_app(None).unwrap();
        app.lsp = Some(hjkl_lsp::LspManager::spawn(hjkl_lsp::LspConfig::default()));

        let buf = {
            let resp = call(&mut app, "nvim_get_current_buf", vec![]);
            assert_ok(resp)
        };
        call(
            &mut app,
            "nvim_buf_set_lines",
            vec![
                buf.clone(),
                Value::from(0i64),
                Value::from(-1i64),
                Value::Boolean(false),
                Value::Array(vec![Value::from("hello world")]),
            ],
        );
        // Reset the dirty-gen bookkeeping so the assertion below proves
        // THIS nvim_buf_set_text call notified the LSP, not the seeding
        // nvim_buf_set_lines above.
        app.active_mut().last_lsp_dirty_gen = None;

        let resp = call(
            &mut app,
            "nvim_buf_set_text",
            vec![
                buf,
                Value::from(0i64),
                Value::from(6i64),
                Value::from(0i64),
                Value::from(11i64),
                Value::Array(vec![Value::from("there")]),
            ],
        );
        assert_ok(resp);

        let dg = app.active_editor().buffer().dirty_gen();
        assert_eq!(
            app.active().last_lsp_dirty_gen,
            Some(dg),
            "nvim_buf_set_text must notify the LSP (didChange)"
        );
        assert!(
            !app.active_editor_mut().take_content_reset(),
            "the content-reset flag must already be drained by the sync helper"
        );

        if let Some(mgr) = app.lsp.take() {
            mgr.shutdown();
        }
    }

    /// The sync chain must also run for a buffer that is NOT the focused
    /// one — `nvim_buf_set_lines`/`nvim_buf_set_text` can target any open
    /// buffer by handle, mirroring `apply_workspace_edit`'s per-slot drain
    /// rather than assuming the active editor.
    #[test]
    fn test_nvim_buf_set_lines_syncs_non_current_buffer() {
        let mut app = build_app(None).unwrap();
        app.lsp = Some(hjkl_lsp::LspManager::spawn(hjkl_lsp::LspConfig::default()));

        // A fresh scratch buffer that is created but never focused.
        let other_buf = {
            let resp = call(
                &mut app,
                "nvim_create_buf",
                vec![Value::Boolean(true), Value::Boolean(false)],
            );
            assert_ok(resp)
        };
        let current_buf = {
            let resp = call(&mut app, "nvim_get_current_buf", vec![]);
            assert_ok(resp)
        };
        assert_ne!(other_buf, current_buf, "sanity: distinct buffers");

        let resp = call(
            &mut app,
            "nvim_buf_set_lines",
            vec![
                other_buf.clone(),
                Value::from(0i64),
                Value::from(-1i64),
                Value::Boolean(false),
                Value::Array(vec![Value::from("side buffer content")]),
            ],
        );
        assert_ok(resp);

        let other_id = match &other_buf {
            Value::Ext(_, bytes) => {
                let mut cursor = std::io::Cursor::new(bytes.as_slice());
                rmpv::decode::read_value(&mut cursor)
                    .unwrap()
                    .as_u64()
                    .unwrap()
            }
            _ => panic!("expected buffer ext handle"),
        };
        let dg = app.nvim_slot_editor(other_id).unwrap().buffer().dirty_gen();
        assert_eq!(
            app.nvim_slot_last_lsp_dirty_gen(other_id),
            Some(dg),
            "the non-focused slot's buffer must ALSO be didChange-notified"
        );
        assert!(
            !app.nvim_slot_editor_mut(other_id)
                .unwrap()
                .take_content_reset(),
            "the non-focused slot's content-reset flag must already be drained"
        );

        if let Some(mgr) = app.lsp.take() {
            mgr.shutdown();
        }
    }

    // ── nvim_get_option_value / nvim_set_option_value ─────────────────────────

    /// Helper: call nvim_set_option_value(name, value, nil).
    fn set_opt(app: &mut crate::app::App, name: &str, value: Value) -> Vec<Value> {
        call(
            app,
            "nvim_set_option_value",
            vec![Value::from(name), value, Value::Nil],
        )
    }

    /// Helper: call nvim_get_option_value(name, nil).
    fn get_opt(app: &mut crate::app::App, name: &str) -> Vec<Value> {
        call(
            app,
            "nvim_get_option_value",
            vec![Value::from(name), Value::Nil],
        )
    }

    #[test]
    fn test_set_bool_option_true_then_get_returns_boolean_true() {
        let mut app = build_app(None).unwrap();

        // Ensure `number` is off first.
        assert_ok(set_opt(&mut app, "number", Value::Boolean(false)));

        // Set to true.
        assert_ok(set_opt(&mut app, "number", Value::Boolean(true)));

        let result = assert_ok(get_opt(&mut app, "number"));
        assert_eq!(
            result,
            Value::Boolean(true),
            "nvim_get_option_value('number') should be Boolean(true)"
        );
    }

    #[test]
    fn test_set_bool_option_false_then_get_returns_boolean_false() {
        let mut app = build_app(None).unwrap();

        // Set number on first, then off.
        assert_ok(set_opt(&mut app, "number", Value::Boolean(true)));
        assert_ok(set_opt(&mut app, "number", Value::Boolean(false)));

        let result = assert_ok(get_opt(&mut app, "number"));
        assert_eq!(
            result,
            Value::Boolean(false),
            "nvim_get_option_value('number') should be Boolean(false)"
        );
    }

    #[test]
    fn test_set_int_option_then_get_returns_integer() {
        let mut app = build_app(None).unwrap();

        assert_ok(set_opt(
            &mut app,
            "shiftwidth",
            Value::Integer(rmpv::Integer::from(8i64)),
        ));

        let result = assert_ok(get_opt(&mut app, "shiftwidth"));
        assert_eq!(
            result,
            Value::Integer(rmpv::Integer::from(8i64)),
            "nvim_get_option_value('shiftwidth') should be Integer(8)"
        );
    }

    #[test]
    fn test_set_string_option_then_get_returns_unquoted_string() {
        let mut app = build_app(None).unwrap();

        assert_ok(set_opt(&mut app, "makeprg", Value::from("cargo build")));

        let result = assert_ok(get_opt(&mut app, "makeprg"));
        assert_eq!(
            result,
            Value::from("cargo build"),
            "nvim_get_option_value('makeprg') should return 'cargo build' (unquoted)"
        );
    }

    #[test]
    fn test_get_unknown_option_returns_error() {
        let mut app = build_app(None).unwrap();

        let resp = get_opt(&mut app, "totally_unknown_option_xyz");
        // resp[2] must NOT be Nil (it should be an error array).
        assert_ne!(
            resp[2],
            Value::Nil,
            "nvim_get_option_value of unknown option must return an error"
        );
    }

    #[test]
    fn test_set_unknown_option_returns_error() {
        let mut app = build_app(None).unwrap();

        let resp = set_opt(&mut app, "totally_unknown_option_xyz", Value::Boolean(true));
        assert_ne!(
            resp[2],
            Value::Nil,
            "nvim_set_option_value of unknown option must return an error"
        );
    }

    #[test]
    fn test_coerce_option_display_on_off() {
        assert_eq!(coerce_option_display("on"), Value::Boolean(true));
        assert_eq!(coerce_option_display("off"), Value::Boolean(false));
    }

    #[test]
    fn test_coerce_option_display_integer() {
        assert_eq!(
            coerce_option_display("8"),
            Value::Integer(rmpv::Integer::from(8i64))
        );
        assert_eq!(
            coerce_option_display("0"),
            Value::Integer(rmpv::Integer::from(0i64))
        );
    }

    #[test]
    fn test_coerce_option_display_quoted_string() {
        // query_option_value returns `"cargo build"` (with outer quotes) for makeprg.
        assert_eq!(
            coerce_option_display("\"cargo build\""),
            Value::from("cargo build")
        );
        // Empty quoted string.
        assert_eq!(coerce_option_display("\"\""), Value::from(""));
    }

    #[test]
    fn test_coerce_option_display_unquoted_string_passthrough() {
        // Non-boolean, non-integer, not quoted → pass through.
        assert_eq!(coerce_option_display("auto"), Value::from("auto"));
        assert_eq!(coerce_option_display("manual"), Value::from("manual"));
    }

    // ── nvim_set_var / nvim_get_var / nvim_del_var (global g:) ───────────────

    #[test]
    fn test_nvim_gvar_string_roundtrip() {
        let mut app = build_app(None).unwrap();

        // set "mykey" = "hello"
        {
            let resp = call(
                &mut app,
                "nvim_set_var",
                vec![Value::from("mykey"), Value::from("hello")],
            );
            assert_ok(resp);
        }

        // get "mykey" should return "hello"
        let result = {
            let resp = call(&mut app, "nvim_get_var", vec![Value::from("mykey")]);
            assert_ok(resp)
        };
        assert_eq!(result, Value::from("hello"), "gvar string round-trip");
    }

    #[test]
    fn test_nvim_gvar_integer_roundtrip() {
        let mut app = build_app(None).unwrap();

        {
            let resp = call(
                &mut app,
                "nvim_set_var",
                vec![
                    Value::from("counter"),
                    Value::Integer(rmpv::Integer::from(42i64)),
                ],
            );
            assert_ok(resp);
        }

        let result = {
            let resp = call(&mut app, "nvim_get_var", vec![Value::from("counter")]);
            assert_ok(resp)
        };
        assert_eq!(
            result,
            Value::Integer(rmpv::Integer::from(42i64)),
            "gvar integer round-trip"
        );
    }

    #[test]
    fn test_nvim_gvar_get_missing_returns_error() {
        let mut app = build_app(None).unwrap();

        let resp = call(&mut app, "nvim_get_var", vec![Value::from("no_such_key")]);
        assert_ne!(resp[2], Value::Nil, "get missing gvar should return error");
    }

    #[test]
    fn test_nvim_gvar_del_then_get_returns_error() {
        let mut app = build_app(None).unwrap();

        // set, del, get → error
        assert_ok(call(
            &mut app,
            "nvim_set_var",
            vec![Value::from("tmp"), Value::from("x")],
        ));
        assert_ok(call(&mut app, "nvim_del_var", vec![Value::from("tmp")]));
        let resp = call(&mut app, "nvim_get_var", vec![Value::from("tmp")]);
        assert_ne!(resp[2], Value::Nil, "get after del should return error");
    }

    // ── nvim_buf_set_var / nvim_buf_get_var / nvim_buf_del_var (b:) ─────────

    #[test]
    fn test_nvim_bvar_roundtrip_on_current_buf() {
        let mut app = build_app(None).unwrap();

        let cur_buf = {
            let resp = call(&mut app, "nvim_get_current_buf", vec![]);
            assert_ok(resp)
        };

        // set var on current buffer
        {
            let resp = call(
                &mut app,
                "nvim_buf_set_var",
                vec![
                    cur_buf.clone(),
                    Value::from("bufkey"),
                    Value::from("bufval"),
                ],
            );
            assert_ok(resp);
        }

        // get var back
        let result = {
            let resp = call(
                &mut app,
                "nvim_buf_get_var",
                vec![cur_buf, Value::from("bufkey")],
            );
            assert_ok(resp)
        };
        assert_eq!(
            result,
            Value::from("bufval"),
            "bvar round-trip on current buf"
        );
    }

    #[test]
    fn test_nvim_bvar_isolation_across_buffers() {
        let mut app = build_app(None).unwrap();

        // Get current (buf A) handle.
        let buf_a = {
            let resp = call(&mut app, "nvim_get_current_buf", vec![]);
            assert_ok(resp)
        };

        // Create a second buffer (buf B).
        let buf_b = {
            let resp = call(
                &mut app,
                "nvim_create_buf",
                vec![Value::Boolean(true), Value::Boolean(false)],
            );
            assert_ok(resp)
        };

        // Set a var on buf A.
        assert_ok(call(
            &mut app,
            "nvim_buf_set_var",
            vec![
                buf_a.clone(),
                Value::from("shared_name"),
                Value::from("value_a"),
            ],
        ));

        // Set a DIFFERENT value for the same name on buf B.
        assert_ok(call(
            &mut app,
            "nvim_buf_set_var",
            vec![
                buf_b.clone(),
                Value::from("shared_name"),
                Value::from("value_b"),
            ],
        ));

        // buf A's var should still be "value_a".
        let val_a = assert_ok(call(
            &mut app,
            "nvim_buf_get_var",
            vec![buf_a, Value::from("shared_name")],
        ));
        assert_eq!(val_a, Value::from("value_a"), "buf A var should be value_a");

        // buf B's var should be "value_b".
        let val_b = assert_ok(call(
            &mut app,
            "nvim_buf_get_var",
            vec![buf_b, Value::from("shared_name")],
        ));
        assert_eq!(val_b, Value::from("value_b"), "buf B var should be value_b");
    }

    #[test]
    fn test_nvim_bvar_get_missing_returns_error() {
        let mut app = build_app(None).unwrap();

        let cur_buf = {
            let resp = call(&mut app, "nvim_get_current_buf", vec![]);
            assert_ok(resp)
        };

        let resp = call(
            &mut app,
            "nvim_buf_get_var",
            vec![cur_buf, Value::from("no_such_bvar")],
        );
        assert_ne!(resp[2], Value::Nil, "get missing bvar should return error");
    }

    // ── nvim_win_set_var / nvim_win_get_var / nvim_win_del_var (w:) ─────────

    #[test]
    fn test_nvim_wvar_roundtrip_on_current_win() {
        let mut app = build_app(None).unwrap();

        let cur_win = {
            let resp = call(&mut app, "nvim_get_current_win", vec![]);
            assert_ok(resp)
        };

        // set var on current window
        {
            let resp = call(
                &mut app,
                "nvim_win_set_var",
                vec![
                    cur_win.clone(),
                    Value::from("winkey"),
                    Value::from("winval"),
                ],
            );
            assert_ok(resp);
        }

        // get var back
        let result = {
            let resp = call(
                &mut app,
                "nvim_win_get_var",
                vec![cur_win, Value::from("winkey")],
            );
            assert_ok(resp)
        };
        assert_eq!(
            result,
            Value::from("winval"),
            "wvar round-trip on current win"
        );
    }

    #[test]
    fn test_nvim_wvar_get_missing_returns_error() {
        let mut app = build_app(None).unwrap();

        let cur_win = {
            let resp = call(&mut app, "nvim_get_current_win", vec![]);
            assert_ok(resp)
        };

        let resp = call(
            &mut app,
            "nvim_win_get_var",
            vec![cur_win, Value::from("no_such_wvar")],
        );
        assert_ne!(resp[2], Value::Nil, "get missing wvar should return error");
    }

    // ── Phase C3 tests ────────────────────────────────────────────────────────

    // ── nvim_replace_termcodes ─────────────────────────────────────────────

    #[test]
    fn test_replace_termcodes_cr() {
        assert_eq!(replace_termcodes("<CR>"), "\r");
        assert_eq!(replace_termcodes("<Enter>"), "\r");
        assert_eq!(replace_termcodes("<Return>"), "\r");
        // Case-insensitive.
        assert_eq!(replace_termcodes("<cr>"), "\r");
    }

    #[test]
    fn test_replace_termcodes_esc() {
        assert_eq!(replace_termcodes("<Esc>"), "\x1b");
        assert_eq!(replace_termcodes("<ESC>"), "\x1b");
    }

    #[test]
    fn test_replace_termcodes_lt() {
        // "<lt>" → literal "<"; "a<lt>b" → "a<b"
        assert_eq!(replace_termcodes("a<lt>b"), "a<b");
    }

    #[test]
    fn test_replace_termcodes_ctrl_a() {
        // <C-a> → 0x01
        assert_eq!(replace_termcodes("<C-a>"), "\x01");
        // <C-z> → 0x1A
        assert_eq!(replace_termcodes("<C-z>"), "\x1a");
    }

    #[test]
    fn test_replace_termcodes_unknown_tag_passthrough() {
        // Unrecognised tags are left as-is.
        assert_eq!(replace_termcodes("<F1>"), "<F1>");
        assert_eq!(replace_termcodes("<M-a>"), "<M-a>");
    }

    // via dispatch
    #[test]
    fn test_nvim_replace_termcodes_via_api() {
        let mut app = build_app(None).unwrap();

        let resp = call(
            &mut app,
            "nvim_replace_termcodes",
            vec![
                Value::from("<CR>"),
                Value::Boolean(false),
                Value::Boolean(false),
                Value::Boolean(true),
            ],
        );
        let result = match assert_ok(resp) {
            Value::String(s) => s.as_str().unwrap_or("").to_owned(),
            other => panic!("expected string, got {other:?}"),
        };
        assert_eq!(result, "\r", "nvim_replace_termcodes('<CR>') should be \\r");
    }

    // ── nvim_exec2 ────────────────────────────────────────────────────────

    #[test]
    fn test_nvim_exec2_set_options() {
        let mut app = build_app(None).unwrap();

        // Execute two set commands via nvim_exec2.
        let resp = call(
            &mut app,
            "nvim_exec2",
            vec![Value::from("set shiftwidth=4\nset number"), Value::Nil],
        );
        // nvim_exec2 returns a dict, not Nil.
        assert_ok(resp);

        // Verify shiftwidth=4 via nvim_get_option_value.
        let sw = assert_ok(call(
            &mut app,
            "nvim_get_option_value",
            vec![Value::from("shiftwidth"), Value::Nil],
        ));
        assert_eq!(
            sw,
            Value::Integer(rmpv::Integer::from(4i64)),
            "shiftwidth should be 4 after nvim_exec2"
        );

        // Verify number=true via nvim_get_option_value.
        let nu = assert_ok(call(
            &mut app,
            "nvim_get_option_value",
            vec![Value::from("number"), Value::Nil],
        ));
        assert_eq!(
            nu,
            Value::Boolean(true),
            "number should be true after nvim_exec2"
        );
    }

    #[test]
    fn test_nvim_exec2_returns_dict() {
        let mut app = build_app(None).unwrap();

        // Without output option → empty map.
        let result = assert_ok(call(
            &mut app,
            "nvim_exec2",
            vec![Value::from("set tabstop=2"), Value::Nil],
        ));
        match &result {
            Value::Map(m) => assert!(m.is_empty(), "no opts → empty map"),
            other => panic!("expected map, got {other:?}"),
        }

        // With output=true → map with "output" key.
        let result2 = assert_ok(call(
            &mut app,
            "nvim_exec2",
            vec![
                Value::from("set tabstop=2"),
                Value::Map(vec![(Value::from("output"), Value::Boolean(true))]),
            ],
        ));
        match &result2 {
            Value::Map(m) => {
                let out = map_get(m, "output");
                assert!(
                    out.is_some(),
                    "opts.output=true → 'output' key should be present"
                );
            }
            other => panic!("expected map, got {other:?}"),
        }
    }

    // ── nvim_feedkeys ─────────────────────────────────────────────────────

    #[test]
    fn test_nvim_feedkeys_insert_x_then_escape() {
        // Start in Normal mode; feed "ix<Esc>" → insert 'x' then return to Normal.
        // The buffer's first line should gain an 'x' at the start.
        let mut app = build_app(None).unwrap();

        // Set a known initial buffer content.
        let buf = assert_ok(call(&mut app, "nvim_get_current_buf", vec![]));
        assert_ok(call(
            &mut app,
            "nvim_buf_set_lines",
            vec![
                buf,
                Value::from(0i64),
                Value::from(-1i64),
                Value::Boolean(false),
                Value::Array(vec![Value::from("hello")]),
            ],
        ));

        // Feed "ix<Esc>" — enter insert, type 'x', escape back to normal.
        let resp = call(
            &mut app,
            "nvim_feedkeys",
            vec![
                Value::from("ix<Esc>"),
                Value::from("n"),
                Value::Boolean(false),
            ],
        );
        assert_ok(resp);

        // The first line should now start with 'x'.
        let line_resp = call(&mut app, "nvim_get_current_line", vec![]);
        let line = match assert_ok(line_resp) {
            Value::String(s) => s.as_str().unwrap_or("").to_owned(),
            other => panic!("expected string, got {other:?}"),
        };
        assert!(
            line.contains('x'),
            "line should contain 'x' after feedkeys('ix<Esc>'), got: {line:?}"
        );
    }

    // ── nvim_set_keymap / nvim_del_keymap ────────────────────────────────

    #[test]
    fn test_nvim_set_keymap_returns_ok() {
        let mut app = build_app(None).unwrap();

        // nvim_set_keymap("n", "Q", "dd", {}) → ok(Nil)
        let resp = call(
            &mut app,
            "nvim_set_keymap",
            vec![
                Value::from("n"),
                Value::from("Q"),
                Value::from("dd"),
                Value::Map(vec![]),
            ],
        );
        assert_ok(resp);
    }

    #[test]
    fn test_nvim_set_keymap_noremap_and_del() {
        let mut app = build_app(None).unwrap();

        // Set a noremap.
        let set_resp = call(
            &mut app,
            "nvim_set_keymap",
            vec![
                Value::from("n"),
                Value::from("Q"),
                Value::from("dd"),
                Value::Map(vec![(Value::from("noremap"), Value::Boolean(true))]),
            ],
        );
        assert_ok(set_resp);

        // The mapping should have been registered (user_keymap_records contains it).
        assert!(
            app.user_keymap_records
                .iter()
                .any(|r| matches!(r.mode, crate::app::keymap::MapMode::Normal)),
            "Normal mode record should exist after set_keymap"
        );

        // Delete the mapping.
        let del_resp = call(
            &mut app,
            "nvim_del_keymap",
            vec![Value::from("n"), Value::from("Q")],
        );
        assert_ok(del_resp);
    }

    #[test]
    fn test_nvim_set_keymap_insert_mode() {
        let mut app = build_app(None).unwrap();

        let resp = call(
            &mut app,
            "nvim_set_keymap",
            vec![
                Value::from("i"),
                Value::from("<C-s>"),
                Value::from("<Esc>:w<CR>"),
                Value::Map(vec![]),
            ],
        );
        assert_ok(resp);

        // Insert-mode record exists.
        assert!(
            app.user_keymap_records
                .iter()
                .any(|r| matches!(r.mode, crate::app::keymap::MapMode::Insert)),
            "Insert mode record should exist"
        );
    }

    // ── getpos / line("v") / col("v") ─────────────────────────────────────

    /// Helper: decode `getpos` result into (bufnum, lnum, col, off).
    fn decode_getpos(v: Value) -> (i64, i64, i64, i64) {
        match v {
            Value::Array(arr) if arr.len() == 4 => {
                let n = |v: &Value| match v {
                    Value::Integer(i) => i.as_i64().unwrap_or(0),
                    _ => panic!("expected integer in getpos result, got {v:?}"),
                };
                (n(&arr[0]), n(&arr[1]), n(&arr[2]), n(&arr[3]))
            }
            other => panic!("getpos: expected 4-element array, got {other:?}"),
        }
    }

    fn call_getpos(app: &mut crate::app::App, expr: &str) -> (i64, i64, i64, i64) {
        let resp = call(
            app,
            "nvim_call_function",
            vec![Value::from("getpos"), Value::Array(vec![Value::from(expr)])],
        );
        decode_getpos(assert_ok(resp))
    }

    fn call_line(app: &mut crate::app::App, expr: &str) -> i64 {
        let resp = call(
            app,
            "nvim_call_function",
            vec![Value::from("line"), Value::Array(vec![Value::from(expr)])],
        );
        match assert_ok(resp) {
            Value::Integer(n) => n.as_i64().unwrap(),
            other => panic!("line({expr}): expected integer, got {other:?}"),
        }
    }

    fn call_col(app: &mut crate::app::App, expr: &str) -> i64 {
        let resp = call(
            app,
            "nvim_call_function",
            vec![Value::from("col"), Value::Array(vec![Value::from(expr)])],
        );
        match assert_ok(resp) {
            Value::Integer(n) => n.as_i64().unwrap(),
            other => panic!("col({expr}): expected integer, got {other:?}"),
        }
    }

    fn call_get_mode(app: &mut crate::app::App) -> String {
        let resp = call(app, "nvim_get_mode", vec![]);
        match assert_ok(resp) {
            Value::Map(pairs) => {
                for (k, v) in pairs {
                    if k == Value::from("mode") {
                        return match v {
                            Value::String(s) => s.as_str().unwrap_or("").to_owned(),
                            other => panic!("mode is not a string: {other:?}"),
                        };
                    }
                }
                panic!("nvim_get_mode: no 'mode' key");
            }
            other => panic!("nvim_get_mode: expected map, got {other:?}"),
        }
    }

    /// Set up a 3-line buffer via nvim_buf_set_lines so we have text to
    /// position the cursor on.
    fn setup_buffer(app: &mut crate::app::App) {
        let buf = {
            let resp = call(app, "nvim_get_current_buf", vec![]);
            assert_ok(resp)
        };
        let lines = Value::Array(vec![
            Value::from("hello world"),
            Value::from("second line"),
            Value::from("third"),
        ]);
        let resp = call(
            app,
            "nvim_buf_set_lines",
            vec![
                buf,
                Value::from(0i64),
                Value::from(-1i64),
                Value::Boolean(false),
                lines,
            ],
        );
        assert_ok(resp);
    }

    #[test]
    fn test_getpos_no_selection_equals_cursor() {
        // With no active visual selection, getpos("v") must equal getpos(".").
        let mut app = build_app(None).unwrap();
        setup_buffer(&mut app);

        // Move cursor to row=1 (2nd line, 0-based), col=3 (0-based).
        // nvim_win_set_cursor takes [lnum(1-based), byte-col].
        // Col 3 is ASCII so byte-col == char-col.
        let win = {
            let resp = call(&mut app, "nvim_get_current_win", vec![]);
            assert_ok(resp)
        };
        {
            let resp = call(
                &mut app,
                "nvim_win_set_cursor",
                vec![
                    win.clone(),
                    Value::Array(vec![Value::from(2i64), Value::from(3i64)]),
                ],
            );
            assert_ok(resp);
        }

        // Normal mode: getpos("v") == getpos(".") == [0, 2, 4, 0] (1-based)
        let pos_dot = call_getpos(&mut app, ".");
        let pos_v = call_getpos(&mut app, "v");
        assert_eq!(pos_dot, (0, 2, 4, 0), "getpos('.') mismatch");
        assert_eq!(
            pos_v, pos_dot,
            "getpos('v') should equal getpos('.') when not in visual"
        );

        // line("v") and col("v") should also agree
        assert_eq!(
            call_line(&mut app, "v"),
            2,
            "line('v') should equal line('.') outside visual"
        );
        assert_eq!(
            call_col(&mut app, "v"),
            4,
            "col('v') should equal col('.') outside visual"
        );
    }

    #[test]
    fn test_getpos_active_visual_selection() {
        // Drive the engine directly into charwise Visual mode:
        //   anchor at (0, 2) → getpos("v") = [0, 1, 3, 0]
        //   cursor at (0, 5) → getpos(".") = [0, 1, 6, 0]
        let mut app = build_app(None).unwrap();
        setup_buffer(&mut app);

        {
            let ed = app.active_editor_mut();
            // Place cursor at (0, 2) and enter Visual — this becomes the anchor.
            ed.set_cursor_doc(0, 2);
            ed.enter_visual_char();
            // Now move the caret to (0, 5); anchor stays at (0, 2).
            ed.set_cursor_doc(0, 5);
        }

        // Verify mode is "v".
        assert_eq!(
            call_get_mode(&mut app),
            "v",
            "nvim_get_mode should return 'v' in Visual"
        );

        // getpos(".") → cursor (0-based row=0, char-col=5) → [0, 1, 6, 0]
        let pos_dot = call_getpos(&mut app, ".");
        assert_eq!(
            pos_dot,
            (0, 1, 6, 0),
            "getpos('.') should be cursor position"
        );

        // getpos("v") → anchor (0-based row=0, char-col=2) → [0, 1, 3, 0]
        let pos_v = call_getpos(&mut app, "v");
        assert_eq!(pos_v, (0, 1, 3, 0), "getpos('v') should be visual anchor");

        // line("v") / col("v") must agree with getpos("v")
        assert_eq!(
            call_line(&mut app, "v"),
            1,
            "line('v') should be anchor row (1-based)"
        );
        assert_eq!(
            call_col(&mut app, "v"),
            3,
            "col('v') should be anchor col (1-based char-col)"
        );

        // Sanity: line(".") / col(".") still agree with getpos(".")
        assert_eq!(
            call_line(&mut app, "."),
            1,
            "line('.') should be cursor row"
        );
        assert_eq!(call_col(&mut app, "."), 6, "col('.') should be cursor col");
    }

    // ── malformed-input hardening (untrusted RPC) ─────────────────────────

    /// A byte-col landing in the middle of a multibyte character must not
    /// panic — it snaps down to the previous char boundary.
    #[test]
    fn test_win_set_cursor_mid_multibyte_col_does_not_panic() {
        let mut app = build_app(None).unwrap();
        // "héllo" — 'é' occupies bytes 1..3, so byte-col 2 is mid-character.
        let resp = call(
            &mut app,
            "nvim_buf_set_lines",
            vec![
                Value::Nil,
                Value::from(0i64),
                Value::from(-1i64),
                Value::Boolean(false),
                Value::Array(vec![Value::from("héllo")]),
            ],
        );
        assert_ok(resp);

        let resp = call(
            &mut app,
            "nvim_win_set_cursor",
            vec![
                Value::Nil,
                Value::Array(vec![Value::from(1i64), Value::from(2i64)]),
            ],
        );
        assert_ok(resp);

        // Cursor snapped down to the char boundary before 'é' → char-col 1
        // → byte-col 1 in nvim_win_get_cursor coordinates.
        let resp = call(&mut app, "nvim_win_get_cursor", vec![Value::Nil]);
        let got = assert_ok(resp);
        assert_eq!(
            got,
            Value::Array(vec![Value::from(1i64), Value::from(1i64)]),
            "mid-char byte-col must snap down to the previous boundary"
        );
    }

    /// Extreme negative row / line-range values from a hostile client must
    /// clamp, not overflow (debug builds would panic on `i64::MIN - 1`).
    #[test]
    fn test_extreme_negative_values_do_not_overflow() {
        // resolve_line_range with i64::MIN must not overflow.
        assert!(resolve_line_range(3, i64::MIN, -1).is_ok());
        assert!(resolve_line_range(0, i64::MIN, i64::MIN).is_ok());

        // nvim_win_set_cursor with row = i64::MIN must not overflow.
        let mut app = build_app(None).unwrap();
        let resp = call(
            &mut app,
            "nvim_win_set_cursor",
            vec![
                Value::Nil,
                Value::Array(vec![Value::from(i64::MIN), Value::from(0i64)]),
            ],
        );
        assert_ok(resp);
    }

    /// setqflist with a negative lnum/col must clamp to line 1 / col 1
    /// instead of wrapping to a huge row.
    #[test]
    fn test_setqflist_negative_lnum_clamps() {
        let mut app = build_app(None).unwrap();
        let entry = Value::Map(vec![
            (Value::from("filename"), Value::from("f.txt")),
            (Value::from("lnum"), Value::from(-5i64)),
            (Value::from("col"), Value::from(-9i64)),
            (Value::from("text"), Value::from("boom")),
        ]);
        let resp = call(
            &mut app,
            "nvim_call_function",
            vec![
                Value::from("setqflist"),
                Value::Array(vec![Value::Array(vec![entry])]),
            ],
        );
        assert_ok(resp);

        let resp = call(
            &mut app,
            "nvim_call_function",
            vec![Value::from("getqflist"), Value::Array(vec![])],
        );
        let got = assert_ok(resp);
        let Value::Array(list) = got else {
            panic!("getqflist must return an array");
        };
        let Value::Map(m) = &list[0] else {
            panic!("qf entry must be a map");
        };
        assert_eq!(
            map_get(m, "lnum"),
            Some(&Value::from(1i64)),
            "negative lnum must clamp to 1"
        );
        assert_eq!(
            map_get(m, "col"),
            Some(&Value::from(1i64)),
            "negative col must clamp to 1"
        );
    }

    /// nvim_buf_set_text with start after end must error, not silently
    /// duplicate the bytes between the two positions.
    #[test]
    fn test_buf_set_text_inverted_range_errors() {
        let mut app = build_app(None).unwrap();
        let resp = call(
            &mut app,
            "nvim_buf_set_lines",
            vec![
                Value::Nil,
                Value::from(0i64),
                Value::from(-1i64),
                Value::Boolean(false),
                Value::Array(vec![Value::from("abc"), Value::from("def")]),
            ],
        );
        assert_ok(resp);

        let resp = call(
            &mut app,
            "nvim_buf_set_text",
            vec![
                Value::Nil,
                Value::from(1i64), // start_row AFTER end_row
                Value::from(0i64),
                Value::from(0i64), // end_row
                Value::from(0i64),
                Value::Array(vec![Value::from("X")]),
            ],
        );
        assert_ne!(resp[2], Value::Nil, "inverted range must be an error");

        // View must be untouched.
        let resp = call(
            &mut app,
            "nvim_buf_get_lines",
            vec![
                Value::Nil,
                Value::from(0i64),
                Value::from(-1i64),
                Value::Boolean(false),
            ],
        );
        let got = assert_ok(resp);
        assert_eq!(
            got,
            Value::Array(vec![Value::from("abc"), Value::from("def")]),
            "buffer must be unchanged after a rejected inverted-range edit"
        );
    }

    /// byte_col_to_char_col: boundary snapping and clamping semantics.
    #[test]
    fn test_byte_col_to_char_col_snapping() {
        // "héllo": h=0, é=1..3, l=3, l=4, o=5.
        assert_eq!(byte_col_to_char_col("héllo", 0), 0);
        assert_eq!(byte_col_to_char_col("héllo", 1), 1);
        assert_eq!(byte_col_to_char_col("héllo", 2), 1, "mid-é snaps down");
        assert_eq!(byte_col_to_char_col("héllo", 3), 2);
        assert_eq!(byte_col_to_char_col("héllo", 999), 5, "clamps to line end");
    }

    // ── run loop teardown (audit R2, fix 2) ────────────────────────────────

    /// Regression: the nvim-api run loop used to `return Ok(0)` straight out
    /// of `run` on every exit path (client closed stdin, protocol error, a
    /// `should_quit` request) WITHOUT calling `app.shutdown()` — orphaning
    /// any attached LSP server — or `app.cleanup_swaps_on_exit()` — leaving
    /// stale swap files that trigger false recovery prompts next launch.
    /// `run_with_io` (the testable body of `run`, parameterized over the
    /// transport) must tear down on every exit path instead.
    #[test]
    fn test_run_with_io_shuts_down_lsp_on_stdin_eof() {
        let mut app = build_app(None).unwrap();
        app.lsp = Some(hjkl_lsp::LspManager::spawn(hjkl_lsp::LspConfig::default()));

        // An empty reader hits EOF on the very first read, exactly like a
        // client that closed stdin before sending anything.
        let mut reader: &[u8] = &[];
        let mut sink = std::io::sink();
        let result = run_with_io(&mut app, &mut reader, &mut sink);

        assert!(
            result.is_ok(),
            "stdin EOF must be a clean exit, got {result:?}"
        );
        assert!(
            app.lsp.is_none(),
            "run_with_io must call app.shutdown() on every exit path — \
             including stdin EOF — so an attached LSP server is never orphaned"
        );
    }

    /// Same regression, but for the `should_quit` exit path (a client sends
    /// a request whose dispatch sets `exit_requested`, e.g. `nvim_command`
    /// with `:q!`) rather than stdin EOF.
    #[test]
    fn test_run_with_io_shuts_down_lsp_on_quit_request() {
        let mut app = build_app(None).unwrap();
        app.lsp = Some(hjkl_lsp::LspManager::spawn(hjkl_lsp::LspConfig::default()));

        // One msgpack-rpc request: nvim_command(":q!") — msgid=1, no
        // trailing bytes, so the SECOND read (if the loop kept going) would
        // hit EOF anyway; the point is that should_quit's `break` must be
        // the path that exits, and teardown must still run.
        let req = Value::Array(vec![
            Value::from(0u64), // request
            Value::from(1u64), // msgid
            Value::from("nvim_command"),
            Value::Array(vec![Value::from("q!")]),
        ]);
        let mut bytes = Vec::new();
        rmpv::encode::write_value(&mut bytes, &req).unwrap();
        let mut reader: &[u8] = bytes.as_slice();
        let mut out = Vec::new();
        let result = run_with_io(&mut app, &mut reader, &mut out);

        assert!(
            result.is_ok(),
            "quit request must be a clean exit, got {result:?}"
        );
        assert!(
            app.lsp.is_none(),
            "run_with_io must call app.shutdown() on the should_quit exit path too"
        );
    }
}
