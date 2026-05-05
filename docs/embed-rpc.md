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
- Phase 3 (`--nvim-api` msgpack-rpc) — forthcoming; will add `nvim_*` aliases
