//! Integration tests for `hjkl --embed` JSON-RPC 2.0 server mode.
//!
//! Spawns the compiled binary, writes JSON-RPC requests to its stdin,
//! reads newline-delimited responses from stdout.

use std::io::{BufRead, BufReader, BufWriter, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

// ---------------------------------------------------------------------------
// EmbedSession helper
// ---------------------------------------------------------------------------

struct EmbedSession {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl EmbedSession {
    fn spawn() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_hjkl"))
            .arg("--embed")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn hjkl --embed");

        let stdin = BufWriter::new(child.stdin.take().expect("child stdin"));
        let stdout = BufReader::new(child.stdout.take().expect("child stdout"));

        EmbedSession {
            child,
            stdin,
            stdout,
            next_id: 1,
        }
    }

    /// Send a request and read the response line (blocks).
    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let req = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": id,
        });
        let mut line = serde_json::to_string(&req).expect("serialize");
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .expect("write request");
        self.stdin.flush().expect("flush");

        let mut resp_line = String::new();
        self.stdout
            .read_line(&mut resp_line)
            .expect("read response");
        serde_json::from_str(resp_line.trim()).expect("parse response JSON")
    }

    /// Send a notification (no id — no response expected).
    #[allow(dead_code)]
    fn notify(&mut self, method: &str, params: Value) {
        let req = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        let mut line = serde_json::to_string(&req).expect("serialize");
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .expect("write notification");
        self.stdin.flush().expect("flush");
    }
}

impl Drop for EmbedSession {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Input "iHello" (insert mode, type Hello), then get_buffer → ["Hello"].
#[test]
fn embed_input_then_get_buffer() {
    let mut s = EmbedSession::spawn();

    let r = s.request("hjkl_input", json!(["iHello"]));
    assert!(r.get("error").is_none(), "hjkl_input failed: {r}");

    let r = s.request("hjkl_get_buffer", json!([]));
    let lines = r["result"].as_array().expect("result is array");
    assert_eq!(lines.len(), 1, "expected 1 line, got {lines:?}");
    assert_eq!(lines[0], "Hello", "buffer mismatch: {lines:?}");
}

/// Pre-load file with "foo bar foo", substitute via hjkl_command, verify.
#[test]
fn embed_command_substitute() {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    use std::io::Write as _;
    f.write_all(b"foo bar foo\n").unwrap();
    let path = f.path().to_path_buf();

    // Spawn with the file path.
    let mut child = Command::new(env!("CARGO_BIN_EXE_hjkl"))
        .arg("--embed")
        .arg(&path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn");

    let stdin = BufWriter::new(child.stdin.take().unwrap());
    let stdout = BufReader::new(child.stdout.take().unwrap());
    let mut s = EmbedSession {
        child,
        stdin,
        stdout,
        next_id: 1,
    };

    let r = s.request("hjkl_command", json!([":%s/foo/baz/g"]));
    assert!(r.get("error").is_none(), "hjkl_command failed: {r}");

    let r = s.request("hjkl_get_buffer", json!([]));
    let lines = r["result"].as_array().expect("result is array");
    assert_eq!(lines[0], "baz bar baz", "substitution mismatch: {lines:?}");
}

/// After "ihello world<Esc>0w" the cursor should be at col 6.
#[test]
fn embed_get_cursor_after_motion() {
    let mut s = EmbedSession::spawn();

    // Type in insert mode, escape, go to start, then word-forward.
    s.request("hjkl_input", json!(["ihello world<Esc>"]));
    s.request("hjkl_input", json!(["0w"]));

    let r = s.request("hjkl_get_cursor", json!([]));
    let arr = r["result"].as_array().expect("result is array");
    assert_eq!(arr[0], 0, "row should be 0, got {arr:?}");
    assert_eq!(arr[1], 6, "col should be 6 (start of 'world'), got {arr:?}");
}

/// get_mode transitions: normal → insert after "i" → normal after "<Esc>".
#[test]
fn embed_get_mode_transitions() {
    let mut s = EmbedSession::spawn();

    let r = s.request("hjkl_get_mode", json!([]));
    assert_eq!(r["result"], "normal", "initial mode: {r}");

    s.request("hjkl_input", json!(["i"]));
    let r = s.request("hjkl_get_mode", json!([]));
    assert_eq!(r["result"], "insert", "after 'i': {r}");

    s.request("hjkl_input", json!(["<Esc>"]));
    let r = s.request("hjkl_get_mode", json!([]));
    assert_eq!(r["result"], "normal", "after '<Esc>': {r}");
}

/// Yank with "0v$y" then read unnamed register — text should contain "Hello".
#[test]
fn embed_register_after_yank() {
    let mut s = EmbedSession::spawn();

    s.request("hjkl_input", json!(["iHello<Esc>"]));
    // Go to col 0, visual select to end of line, yank.
    s.request("hjkl_input", json!(["0v$y"]));

    let r = s.request("hjkl_get_register", json!(["\""]));
    let obj = &r["result"];
    assert!(!obj.is_null(), "register should not be null: {r}");
    let text = obj["text"].as_str().expect("text field");
    assert!(
        text.contains("Hello"),
        "unnamed register text should contain 'Hello', got: {text:?}"
    );
}

/// Unknown method → error.code == -32601.
#[test]
fn embed_unknown_method_returns_jsonrpc_error() {
    let mut s = EmbedSession::spawn();

    let r = s.request("hjkl_nonexistent", json!([]));
    let code = r["error"]["code"].as_i64().expect("error.code");
    assert_eq!(code, -32601, "expected method-not-found: {r}");
}

/// Malformed JSON → parse error (-32700), then next valid request succeeds.
#[test]
fn embed_malformed_json_keeps_loop_alive() {
    let mut s = EmbedSession::spawn();

    // Send bad JSON directly.
    s.stdin.write_all(b"not json\n").expect("write bad line");
    s.stdin.flush().expect("flush");

    // Read the parse-error response.
    let mut resp_line = String::new();
    s.stdout.read_line(&mut resp_line).expect("read error resp");
    let err_resp: Value = serde_json::from_str(resp_line.trim()).expect("parse error resp");
    let code = err_resp["error"]["code"].as_i64().expect("error.code");
    assert_eq!(code, -32700, "expected parse error: {err_resp}");

    // Send a valid request — server must still be alive.
    let r = s.request("hjkl_get_mode", json!([]));
    assert_eq!(r["result"], "normal", "server died after bad JSON: {r}");
}

/// Close stdin → server exits cleanly with code 0 within 2 s.
#[test]
fn embed_eof_exits_clean() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_hjkl"))
        .arg("--embed")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn");

    // Take ownership of stdin/stdout.
    let mut stdin = BufWriter::new(child.stdin.take().expect("stdin"));
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout"));

    // Send a couple of requests manually.
    let req1 = "{\"jsonrpc\":\"2.0\",\"method\":\"hjkl_get_mode\",\"params\":[],\"id\":1}\n";
    stdin.write_all(req1.as_bytes()).expect("write req1");
    stdin.flush().expect("flush");
    let mut _buf = String::new();
    stdout.read_line(&mut _buf).expect("read resp1");

    let req2 = "{\"jsonrpc\":\"2.0\",\"method\":\"hjkl_input\",\"params\":[\"iHi\"],\"id\":2}\n";
    stdin.write_all(req2.as_bytes()).expect("write req2");
    stdin.flush().expect("flush");
    _buf.clear();
    stdout.read_line(&mut _buf).expect("read resp2");

    // Drop stdin → EOF signal.
    drop(stdin);

    // Wait up to 2 seconds for clean exit.
    let start = std::time::Instant::now();
    loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => {
                assert_eq!(status.code(), Some(0), "expected exit 0, got: {status}");
                return;
            }
            None => {
                if start.elapsed().as_secs() >= 2 {
                    let _ = child.kill();
                    panic!("hjkl --embed did not exit within 2 seconds after EOF");
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }
}
