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

### Buffer, window, and tabpage ext-type handles

`nvim-rs` expects handles as `Value::Ext(tag, bytes)`. hjkl uses tag `0` for
buffers, `1` for windows, and `2` for tabpages. The payload is the msgpack
encoding of the id integer itself (id 1 is the positive fixint `0x01`).

- **Buffer** ids start at 1 and increment per buffer. A `Nil`, missing, or `0`
  handle means "current buffer".
- **Window** ids are 0-based indices into the window table, so id `0` is a real
  window and is _not_ remapped to "current"; only `Nil`/missing means current.
- **Tabpage** ids are 0-based indices into the tab list. They are indices, not
  stable handles — they shift when a tab is closed.

Raw integers are accepted anywhere a handle is expected.

### Supported nvim\_\* methods

Methods not listed here respond with a msgpack-rpc error
(`method not implemented: …`).

Index conventions: `nvim_buf_{get,set}_lines` take 0-based `start`/`end` where
`end = -1` means end of buffer and `strict_indexing` is honoured (a missing or
non-boolean value reads as `false`). `nvim_buf_{get,set}_text` rows and cols are
0-based with negatives clamped to `0` — hjkl does not implement nvim's
negative-index addressing there. Cursor rows are 1-based and cursor cols are
byte-cols.

**Buffers**

| Method                                               | Params                                                     | Result           |
| ---------------------------------------------------- | ---------------------------------------------------------- | ---------------- |
| `nvim_get_current_buf()`                             | —                                                          | `Ext(0, id)`     |
| `nvim_list_bufs()`                                   | —                                                          | `Ext(0, id)[]`   |
| `nvim_set_current_buf(buf)`                          | buffer handle                                              | Nil              |
| `nvim_create_buf(listed, scratch)`                   | both arguments are **ignored**; always makes a real buffer | `Ext(0, new_id)` |
| `nvim_buf_get_name(buf)`                             | —                                                          | `String`         |
| `nvim_buf_set_name(buf, name)`                       | —                                                          | Nil              |
| `nvim_buf_line_count(buf)`                           | —                                                          | `i64`            |
| `nvim_buf_get_lines(buf, start, end, strict)`        | —                                                          | `String[]`       |
| `nvim_buf_set_lines(buf, start, end, strict, lines)` | rebuilds the buffer; **resets undo history**               | Nil              |
| `nvim_buf_get_text(buf, srow, scol, erow, ecol, {})` | byte cols; the opts dict is **ignored**                    | `String[]`       |
| `nvim_buf_set_text(buf, srow, scol, erow, ecol, r)`  | byte cols; **resets undo history**                         | Nil              |
| `nvim_get_current_line()`                            | takes no params; always the active buffer                  | `String`         |
| `nvim_set_current_line(line)`                        | active buffer only                                         | Nil              |

**Windows and tabpages**

| Method                            | Params                                            | Result                      |
| --------------------------------- | ------------------------------------------------- | --------------------------- |
| `nvim_get_current_win()`          | —                                                 | `Ext(1, id)`                |
| `nvim_list_wins()`                | —                                                 | `Ext(1, id)[]`              |
| `nvim_set_current_win(win)`       | —                                                 | Nil                         |
| `nvim_win_get_buf(win)`           | —                                                 | `Ext(0, id)`                |
| `nvim_win_set_buf(win, buf)`      | —                                                 | Nil                         |
| `nvim_win_close(win, force)`      | `force` **ignored**; no-op if only one window     | Nil                         |
| `nvim_win_get_cursor(win)`        | —                                                 | `[row (1-based), byte-col]` |
| `nvim_win_set_cursor(win, [r,c])` | 1-based row, byte-col                             | Nil                         |
| `nvim_win_get_height(win)`        | measured against a fixed headless 80×24 area      | `i64`                       |
| `nvim_win_get_width(win)`         | same                                              | `i64`                       |
| `nvim_win_set_height(win, h)`     | best-effort split-ratio nudge; no-op if no parent | Nil                         |
| `nvim_win_set_width(win, w)`      | same                                              | Nil                         |
| `nvim_list_tabpages()`            | —                                                 | `Ext(2, i)[]`               |
| `nvim_get_current_tabpage()`      | —                                                 | `Ext(2, i)`                 |
| `nvim_set_current_tabpage(tab)`   | —                                                 | Nil                         |
| `nvim_tabpage_list_wins(tab)`     | —                                                 | `Ext(1, id)[]`              |
| `nvim_tabpage_is_valid(tab)`      | —                                                 | `bool`                      |

There is no `nvim_open_win` and no tabpage-creation method: new windows and tabs
come from `:split` / `:vsplit` / `:tabnew` via `nvim_command` or `nvim_exec2`.

**Input, commands, and mode**

| Method                                 | Params                                                      | Result                        |
| -------------------------------------- | ----------------------------------------------------------- | ----------------------------- |
| `nvim_input(keys)`                     | vim-key notation: `iHello<Esc>`, `dd`                       | `i64` — byte length of `keys` |
| `nvim_feedkeys(keys, mode, escape_ks)` | same execution path as `nvim_input`; both flags **ignored** | Nil                           |
| `nvim_command(cmd)`                    | ex command, leading `:` optional                            | Nil                           |
| `nvim_exec2(src, opts)`                | see below                                                   | Map                           |
| `nvim_get_mode()`                      | —                                                           | Map `{mode, blocking: false}` |
| `nvim_replace_termcodes(src, ...)`     | trailing three args accepted but **ignored**                | `String`                      |

`nvim_get_mode` emits only the five modes the engine has: `"n"`, `"i"`, `"v"`,
`"V"`, and `"\x16"` (visual block). `blocking` is always `false`.

`nvim_replace_termcodes` handles a subset: `<CR>`/`<Enter>`/`<Return>`, `<Esc>`,
`<Tab>`, `<BS>`, `<Space>`, `<Nul>`, `<lt>`, `<Bar>`, `<Bslash>`, and `<C-x>`
for a single ASCII letter or digit. Anything else — function keys, `<M-…>`,
`<A-…>`, mouse codes — passes through verbatim.

`nvim_exec2` is **not a vimscript interpreter**. It splits `src` on newlines and
runs each non-empty line as one standalone ex command (leading `:` optional);
there is no `function`/`if`/`let` handling and no line continuation. Output
capture is unimplemented: with `opts.output == true` it returns
`{"output": ""}`, otherwise an empty map.

**Options, variables, and keymaps**

| Method                                     | Params                                | Result      |
| ------------------------------------------ | ------------------------------------- | ----------- |
| `nvim_get_option_value(name, opts)`        | `opts` (scope/buf/win) is **ignored** | scalar      |
| `nvim_set_option_value(name, value, {})`   | `opts` **ignored**                    | Nil         |
| `nvim_set_var` / `get_var` / `del_var`     | global variables                      | Nil / value |
| `nvim_buf_set_var` / `get_var` / `del_var` | keyed by buffer id                    | Nil / value |
| `nvim_win_set_var` / `get_var` / `del_var` | keyed by window id                    | Nil / value |
| `nvim_set_keymap(mode, lhs, rhs, opts)`    | only `opts.noremap` is read           | Nil         |
| `nvim_del_keymap(mode, lhs)`               | —                                     | Nil         |

Option access is gated on hjkl's own option registry (the same set `:set`
accepts, aliases included); any other name errors with `unknown option: {name}`.
Both calls always act on the **active** editor regardless of the `opts` dict.
Values map onto `:set` tokens: `true` → `name`, `false` → `noname`, an integer →
`name=n`, a string → `name=s`.

`nvim_set_keymap` honours `noremap` only — `silent`, `expr`, `desc`, `nowait`,
`unique`, and `callback` are ignored. Modes `n`, `i`, `v`, `x`, `o` map to their
ex-command prefixes; every other mode string (including `s`, `t`, `c`, and
multi-char forms like `"nv"`) falls back to the unprefixed `map` / `noremap`.
Both keymap calls return Nil unconditionally — a rejected mapping is not visible
in the return value.

**`nvim_call_function`**

Eleven function names are supported; anything else errors with
`nvim_call_function: unsupported function: {name}`.

| Function                | Behaviour                                                                                               |
| ----------------------- | ------------------------------------------------------------------------------------------------------- |
| `getreg(reg)`           | register contents as `String`; unset register → `""`                                                    |
| `getqflist()`           | `{bufnr, lnum, col, text, valid}` maps; the `{what}` dict form is not supported                         |
| `getloclist(win)`       | `win` **ignored** — there is one global location list                                                   |
| `setqflist(list, ...)`  | always a full replace; `action` and `what` **ignored**; returns `0`                                     |
| `setloclist(win, list)` | `win` **ignored**; returns `0`                                                                          |
| `bufnr(expr)`           | `""`/`"%"`/absent → current; `"$"` → highest buffer id; other strings substring-match a name; else `-1` |
| `bufname(expr)`         | as above, returning the name or `""`                                                                    |
| `expand(expr)`          | only `%`, `%:p`, `%:t`, `%:h`, `%:e`, `%:r`; everything else (incl. `<cword>`, `#`, `$VAR`) → `""`      |
| `line(expr)`            | `"."`, `"$"`, `"v"` only; any other expression → `0`                                                    |
| `col(expr)`             | `"."`, `"$"`, `"v"` only; any other expression → `0`. Returns a **char**-col, unlike vim's byte-col     |
| `getpos(expr)`          | `[0, lnum, col, 0]`; only `"v"` is special-cased, everything else falls back to the cursor position     |

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

The `hjkl-compat-oracle` includes a `nvim_api_tier_passes` test that drives the
cases in `corpus/nvim_api_tier.toml` through `hjkl --nvim-api` rather than the
in-process key-replay driver. These cases pass via the nvim-api path (ex
commands route through `ex::run`) but diverge in-process (the vim FSM does not
handle `:` keystrokes), so they graduated out of `known_divergences.toml` (now
empty) into their own tier. Enable the test with:

```sh
HJKL_ORACLE_NVIM_API=1 cargo test -p hjkl-compat-oracle nvim_api_tier_passes
```
