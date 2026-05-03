# hjkl-clipboard

In-house cross-platform clipboard library for the hjkl ecosystem — rich MIME
types, async support, and OSC 52 fallback for SSH sessions.

[![CI](https://github.com/kryptic-sh/hjkl-clipboard/actions/workflows/ci.yml/badge.svg)](https://github.com/kryptic-sh/hjkl-clipboard/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/hjkl-clipboard.svg)](https://crates.io/crates/hjkl-clipboard)
[![docs.rs](https://img.shields.io/docsrs/hjkl-clipboard)](https://docs.rs/hjkl-clipboard)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Website](https://img.shields.io/badge/website-hjkl.kryptic.sh-7ee787)](https://hjkl.kryptic.sh)

No `arboard` dependency. Hand-rolled backends for every supported platform give
real selection ownership, rich MIME types, and an async API that doesn't require
picking a runtime.

## Platform support

| Platform          | Backend                                         |
| ----------------- | ----------------------------------------------- |
| Linux Wayland     | `ext_data_control_v1` (KDE 6.2+, wlroots, etc.) |
| Linux Wayland     | `zwp_primary_selection_v1` for PRIMARY          |
| Linux X11         | XCB via `dlopen`, INCR + auto-`SAVE_TARGETS`    |
| macOS             | `NSPasteboard` via raw `objc_msgSend`           |
| Windows           | Win32 `CF_UNICODETEXT`, `CF_DIBV5` + PNG        |
| OSC 52 (fallback) | SSH sessions, GNOME, any TTY                    |

## Quick start

Add to `Cargo.toml`:

```toml
hjkl-clipboard = "0.4"
```

### Sync API

```rust
use hjkl_clipboard::{Clipboard, ClipboardError, MimeType, Selection};

fn main() -> Result<(), ClipboardError> {
    let cb = Clipboard::new()?;

    // Write plain text to the system clipboard
    cb.set(Selection::Clipboard, MimeType::Text, b"hello from hjkl")?;

    // Read it back
    let bytes = cb.get(Selection::Clipboard, MimeType::Text)?;
    println!("{}", String::from_utf8_lossy(&bytes));

    // Write HTML
    cb.set(
        Selection::Clipboard,
        MimeType::Html,
        b"<b>bold</b>",
    )?;

    // Write a PNG image
    let png_bytes: Vec<u8> = std::fs::read("icon.png")?;
    cb.set(Selection::Clipboard, MimeType::Png, &png_bytes)?;

    // What MIME types are currently on the clipboard?
    let available = cb.available(Selection::Clipboard)?;
    println!("available: {available:?}");

    // Clear the clipboard
    cb.clear(Selection::Clipboard)?;

    Ok(())
}
```

### Async API (runtime-agnostic)

See [`examples/async_basic.rs`](examples/async_basic.rs) for a runnable version
using `pollster` as a zero-dep executor.

```rust
use hjkl_clipboard::{Clipboard, ClipboardError, MimeType, Selection};

async fn clipboard_demo() -> Result<(), ClipboardError> {
    let cb = Clipboard::new()?;

    cb.set_async(Selection::Clipboard, MimeType::Text, b"async write").await?;
    let text = cb.get_async(Selection::Clipboard, MimeType::Text).await?;
    println!("{}", String::from_utf8_lossy(&text));

    let mimes = cb.available_async(Selection::Clipboard).await?;
    println!("mimes: {mimes:?}");

    cb.clear_async(Selection::Clipboard).await?;

    Ok(())
}
```

### Custom MIME types and URI list helpers

See [`examples/custom_mime.rs`](examples/custom_mime.rs) for a runnable version.

```rust
use hjkl_clipboard::{Clipboard, ClipboardError, MimeType, Selection, Uri};
use std::path::PathBuf;

fn main() -> Result<(), ClipboardError> {
    let cb = Clipboard::new()?;

    // Raw passthrough — no translation applied.
    cb.set(
        Selection::Clipboard,
        MimeType::Custom("application/x-my-format".into()),
        b"\x00\x01\x02",
    )?;

    let data = cb.get(
        Selection::Clipboard,
        MimeType::Custom("application/x-my-format".into()),
    )?;
    println!("custom: {} bytes", data.len());

    // URI list.
    let uris = vec![
        Uri::File(PathBuf::from("/home/user/document.pdf")),
        Uri::Other("https://example.com".into()),
    ];
    cb.set_uri_list(Selection::Clipboard, &uris)?;

    let back = cb.get_uri_list(Selection::Clipboard)?;
    println!("uris: {back:?}");

    Ok(())
}
```

### Backend detection (diagnostics)

See [`examples/backend_detect.rs`](examples/backend_detect.rs) for a runnable
version.

```rust
use hjkl_clipboard::{BackendKind, Capabilities, Clipboard, ClipboardError};

fn main() -> Result<(), ClipboardError> {
    let cb = Clipboard::new()?;
    println!("backend: {}", cb.kind());
    let caps = cb.capabilities();
    if !caps.contains(Capabilities::READ) {
        println!("note: this backend is write-only");
    }
    if caps.contains(Capabilities::ASYNC_WRITE) {
        println!("note: native async writes available");
    }
    if cb.kind() == BackendKind::Osc52 {
        println!("note: OSC 52 fell back — likely SSH / no display");
    }
    Ok(())
}
```

### Capability-gated calls

`Capabilities` is cheap to query and lets callers skip ops that would just
return `UnsupportedMime` or `UnsupportedAsync` from the backend (Wayland / X11
calls go through a thread hop):

```rust
use hjkl_clipboard::{Capabilities, Clipboard, ClipboardError, MimeType, Selection};

fn maybe_read(cb: &Clipboard) -> Option<Vec<u8>> {
    if !cb.capabilities().contains(Capabilities::READ) {
        return None;
    }
    cb.get(Selection::Clipboard, MimeType::Text).ok()
}
```

### Custom backends

`Backend` is a public trait. Implement it directly, or use one of the bundled
extension types:

- `backend::mock::MockBackend` — in-memory test backend with configurable
  `kind` + `capabilities`. Records `set` / `clear`, programmable `get` /
  `available` responses. Both sync and async paths.
- `backend::ssh_aware::SshAwareBackend` — decorator that wraps any
  `Box<dyn Backend>` and falls back to OSC 52 on write failures
  (`BackendUnavailable` / `UnsupportedMime` / `NoDisplay` / `FocusRequired`).
  Capabilities are the union of inner + OSC 52.

```rust
use hjkl_clipboard::{BackendKind, Capabilities, Clipboard, MimeType, Selection};
use hjkl_clipboard::backend::mock::MockBackend;

let mock = MockBackend::new(BackendKind::Mock, Capabilities::all());
mock.preset_get(Selection::Clipboard, MimeType::Text, Ok(b"hi".to_vec()));

let handle = mock.handle();
let cb = Clipboard::with_backend(Box::new(mock));
cb.set(Selection::Clipboard, MimeType::Text, b"world").unwrap();
assert_eq!(cb.get(Selection::Clipboard, MimeType::Text).unwrap(), b"hi");
assert_eq!(handle.set_calls().len(), 1);
```

### PRIMARY selection (Linux only)

On Linux both `Selection::Clipboard` and `Selection::Primary` are supported.
Other platforms return `UnsupportedMime` for PRIMARY ops.

```rust
use hjkl_clipboard::{Clipboard, ClipboardError, MimeType, Selection};

fn main() -> Result<(), ClipboardError> {
    let cb = Clipboard::new()?;
    cb.set(Selection::Primary, MimeType::Text, b"selected text")?;
    Ok(())
}
```

## Caveats

### GNOME / no `ext_data_control_v1`

GNOME's Mutter compositor does not implement the `ext_data_control_v1` (or
`wlr_data_control_v1`) protocol. Without the data-control extension the library
cannot own a Wayland selection. When running in a TTY (SSH or local terminal)
the library falls back to OSC 52 automatically. When running inside a GNOME
session with no TTY available, `Clipboard::new()` returns
`ClipboardError::FocusRequired`.

### OSC 52 paste over SSH

OSC 52 is a _write-only_ path. There is no standardized way to request the
terminal's clipboard back over the escape sequence; `get` and `available` return
`UnsupportedMime` for the OSC 52 backend.

Paste support (the terminal echoing OSC 52 clipboard contents _back_ to the
application) requires opt-in terminal configuration:

| Terminal    | Paste support |
| ----------- | ------------- |
| kitty       | Yes (default) |
| WezTerm     | Yes (default) |
| iTerm2      | Yes (opt-in)  |
| xterm       | No            |
| most others | No            |

### OSC 52 payload cap

OSC 52 payloads are capped at ~74 000 base64 bytes (a widely accepted safe
limit). Larger payloads return `ClipboardError::PayloadTooLarge`.

### macOS / Windows async

`set_async` / `get_async` / `clear_async` / `available_async` on macOS and
Windows use the native backend (NSPasteboard / Win32). Because both backends are
synchronous, the async methods wrap the call in `std::future::ready` — they
complete without spawning a thread and return immediately without yielding to
the executor. This is correct and efficient for the call frequency typical of
clipboard use.

## Build prerequisites

| Platform | Requirement                                                   |
| -------- | ------------------------------------------------------------- |
| Linux    | `libxcb.so.1` at runtime (loaded via `dlopen`, not link-time) |
| macOS    | None — AppKit/Foundation/libobjc are system frameworks        |
| Windows  | None — user32/kernel32 are always present                     |

On Arch: `pacman -S libxcb`. On Debian/Ubuntu: `apt install libxcb1`.

## MSRV

Rust **1.95** (Edition 2024). Bumps land freely when useful; every bump is
documented in [`CHANGELOG.md`](CHANGELOG.md).

## Feature flags

None currently. All backends are compiled in unconditionally for each supported
target.

## License

MIT. See [LICENSE](LICENSE).
