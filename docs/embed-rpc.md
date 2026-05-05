# hjkl --embed: JSON-RPC 2.0 server

Phase 2 of [issue #26](https://github.com/kryptic-sh/hjkl/issues/26).

## Overview

`hjkl --embed` boots without a TUI and speaks JSON-RPC 2.0 over stdin/stdout.
External programs — test harnesses, editor integrations, scripts — can drive a
live `Editor` FSM: feed keystrokes, dispatch ex commands, and query buffer
state, cursor position, mode, and registers over the wire.

## Wire format

- One JSON object per line on stdin → one JSON object per line on stdout.
- Newline-delimited (no length-prefix framing).
- The server always flushes stdout after each response.
- Responses are written before the next request is read.
- EOF on stdin → server exits with code `0`.
- Notifications (requests without `"id"`) are dispatched but produce no
  response.

## Method catalogue

| Method              | Params                                                              | Result                                                                               |
| ------------------- | ------------------------------------------------------------------- | ------------------------------------------------------------------------------------ |
| `hjkl_input`        | `[keys: string]` — vim-key notation: `iHello<Esc>`, `dd`, `:wq<CR>` | `null`                                                                               |
| `hjkl_command`      | `[cmd: string]` — ex command without leading `:`                    | `null` on success; JSON-RPC error on failure                                         |
| `hjkl_get_buffer`   | `[]`                                                                | `string[]` — one entry per line, no trailing newlines                                |
| `hjkl_set_buffer`   | `[lines: string[]]`                                                 | `null` — replaces full buffer                                                        |
| `hjkl_get_cursor`   | `[]`                                                                | `[row: number, col: number]` — 0-based                                               |
| `hjkl_set_cursor`   | `[row: number, col: number]`                                        | `null`                                                                               |
| `hjkl_get_mode`     | `[]`                                                                | `string` — `"normal"` / `"insert"` / `"visual"` / `"visual_line"` / `"visual_block"` |
| `hjkl_get_register` | `[reg: string]` — single char                                       | `{"text": string, "linewise": bool}` or `null` if empty                              |

## Error codes

| Code     | Meaning                                                                       |
| -------- | ----------------------------------------------------------------------------- |
| `-32700` | Parse error — request is not valid JSON                                       |
| `-32600` | Invalid Request — missing `jsonrpc` or `method` field                         |
| `-32601` | Method not found                                                              |
| `-32602` | Invalid params — wrong type or missing required element                       |
| `-32000` | Ex-command failure — `:wq` to an unnamed buffer, bad substitute pattern, etc. |

## Examples

**Type text and read the buffer**

```
→ {"jsonrpc":"2.0","method":"hjkl_input","params":["iHello world<Esc>"],"id":1}
← {"jsonrpc":"2.0","result":null,"id":1}

→ {"jsonrpc":"2.0","method":"hjkl_get_buffer","params":[],"id":2}
← {"jsonrpc":"2.0","result":["Hello world"],"id":2}
```

**Run a substitute command**

```
→ {"jsonrpc":"2.0","method":"hjkl_command","params":[":%s/world/hjkl/g"],"id":3}
← {"jsonrpc":"2.0","result":null,"id":3}

→ {"jsonrpc":"2.0","method":"hjkl_get_buffer","params":[],"id":4}
← {"jsonrpc":"2.0","result":["Hello hjkl"],"id":4}
```

**Query cursor and mode**

```
→ {"jsonrpc":"2.0","method":"hjkl_get_cursor","params":[],"id":5}
← {"jsonrpc":"2.0","result":[0,10],"id":5}

→ {"jsonrpc":"2.0","method":"hjkl_get_mode","params":[],"id":6}
← {"jsonrpc":"2.0","result":"normal","id":6}
```

**Read a register**

```
→ {"jsonrpc":"2.0","method":"hjkl_input","params":["0v$y"],"id":7}
← {"jsonrpc":"2.0","result":null,"id":7}

→ {"jsonrpc":"2.0","method":"hjkl_get_register","params":["\""],"id":8}
← {"jsonrpc":"2.0","result":{"text":"Hello hjkl","linewise":false},"id":8}
```

## Reference

- Phase 1 (`--headless +cmd`) — commit f632184
- Phase 2 (`--embed` JSON-RPC) —
  [issue #26](https://github.com/kryptic-sh/hjkl/issues/26)
- Phase 3 (`--nvim-api` msgpack-rpc) —
  [issue #26](https://github.com/kryptic-sh/hjkl/issues/26)

---

## nvim-api mode

Phase 3 of [issue #26](https://github.com/kryptic-sh/hjkl/issues/26).

`hjkl --nvim-api` boots without a TUI and speaks the
[msgpack-rpc protocol](https://github.com/msgpack-rpc/msgpack-rpc/blob/master/spec.md)
over stdin/stdout using **nvim-compatible method names**. Existing `nvim-rs`
clients can target `hjkl --nvim-api` as a drop-in subprocess replacement for
`nvim --headless --embed`.

### Wire format

Messages are bare msgpack values (no length-prefix framing):

| Direction    | Format                                                   |
| ------------ | -------------------------------------------------------- |
| Request      | `[0, msgid: u32, method: String, params: Array]`         |
| Response     | `[1, msgid: u32, error: Value\|Nil, result: Value\|Nil]` |
| Notification | `[2, method: String, params: Array]`                     |

The server reads messages from stdin in a loop. Responses are written to stdout
and flushed after each one. EOF on stdin → server exits with code `0`.

### Buffer and window ext-type handles

`nvim-rs` expects buffer handles as `Value::Ext(0, bytes)` and window handles as
`Value::Ext(1, bytes)`. hjkl is single-buffer; both handles carry id=1 encoded
as a msgpack positive fixint (`0x01`).

### Supported nvim\_\* methods

| Method                                               | Params                                        | Result                                                    |
| ---------------------------------------------------- | --------------------------------------------- | --------------------------------------------------------- |
| `nvim_get_current_buf()`                             | —                                             | Ext(0, 1) buffer handle                                   |
| `nvim_get_current_win()`                             | —                                             | Ext(1, 1) window handle                                   |
| `nvim_buf_set_lines(buf, start, end, strict, lines)` | 0-based start/end; end=-1 means end of buffer | Nil                                                       |
| `nvim_buf_get_lines(buf, start, end, strict)`        | 0-based start/end; end=-1 means end of buffer | `String[]`                                                |
| `nvim_win_set_cursor(win, [row, col])`               | row is 1-based; col is byte-col               | Nil                                                       |
| `nvim_win_get_cursor(win)`                           | —                                             | `[row: i64, col: i64]` (1-based row, byte-col)            |
| `nvim_input(keys)`                                   | vim-key notation: `iHello<Esc>`, `dd`         | i64 (bytes consumed)                                      |
| `nvim_command(cmd)`                                  | ex command with or without leading `:`        | Nil on success; msgpack-rpc error on failure              |
| `nvim_get_mode()`                                    | —                                             | Map `{mode: "n"\|"i"\|"v"\|"V"\|"\x16", blocking: false}` |
| `nvim_call_function("getreg", [reg])`                | only `"getreg"` supported                     | String register contents                                  |

Methods not in this list respond with a msgpack-rpc error.

### Usage with nvim-rs

```rust
use nvim_rs::{create::tokio as create, Handler};
use tokio::process::Command;

let mut cmd = Command::new("hjkl");
cmd.arg("--nvim-api");
let (nvim, _io, _child) = create::new_child_cmd(&mut cmd, NoopHandler).await?;

let buf = nvim.get_current_buf().await?;
buf.set_lines(0, -1, false, vec!["hello".to_string()]).await?;
let lines = buf.get_lines(0, -1, false).await?;
assert_eq!(lines, vec!["hello"]);
```

### compat-oracle integration

The `hjkl-compat-oracle` includes a `substitute_via_nvim_api` test that drives
the 4 substitute cases from `known_divergences.toml` through `hjkl --nvim-api`
rather than the in-process key-replay driver. These cases pass via the nvim-api
path (ex commands route through `ex::run`) but diverge in-process (the vim FSM
does not handle `:` keystrokes). Enable the test with:

```sh
HJKL_ORACLE_NVIM_API=1 cargo test -p hjkl-compat-oracle substitute_via_nvim_api
```
