# Changelog

All notable changes to this project will be documented in this file. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). This
project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.4.2] - 2026-05-03

### Added

- **`Clipboard::backend_name()`** — returns a stable `&'static str` identifier
  for the active backend (`"wayland"`, `"x11"`, `"macos"`, `"windows"`,
  `"osc52"`). Useful for diagnostics and detecting silent OSC 52 fallback when
  no display server is reachable.

### Changed

- **`ClipboardError` is now `#[non_exhaustive]`** — downstream code that matches
  exhaustively on `ClipboardError` will need a wildcard (`_ => …`) arm. This is
  pre-1.0 stability hardening so future variants can be added without a breaking
  change.

### Fixed

- **OSC 52 size-cap detection no longer relies on `io::ErrorKind::Other`
  heuristic** — `Osc52Backend::set_inner` now checks the base64-encoded length
  against `OSC52_MAX` before calling `write_osc52`, returning
  `ClipboardError::PayloadTooLarge` directly. `write_osc52` itself no longer
  performs the check and will only fail with genuine I/O errors.

## [0.4.1] - 2026-05-03

### Fixed

- **macOS/Windows backends now selected by `Clipboard::new()`** — 0.4.0
  regressed to OSC 52 on every non-Linux platform. `Clipboard::new()` now
  returns the `MacosBackend` on macOS and `WindowsBackend` on Windows.
- **Autorelease pool on macOS** — all `MacosBackend` methods now wrap their body
  in `objc_autoreleasePoolPush` / `objc_autoreleasePoolPop` via a `Drop`-based
  guard. Without an explicit pool on non-main threads, autoreleased
  `NSData`/`NSString`/`NSArray` objects would accumulate indefinitely.
- **macOS/Windows async no longer panics** — `set_async`, `get_async`,
  `clear_async`, `available_async` on macOS and Windows are now wired to the
  native backends (sync-wrapped in `std::future::ready`). The 0.4.0 arms
  previously called `unimplemented!()`.
- **`Clipboard: Clone`** — the 0.4.0 changelog advertised `Clipboard` as
  clonable but `#[derive(Clone)]` was missing. Added.
- **README accuracy** — removed the incorrect claim that macOS/Windows async
  panics with `unimplemented!()`.

### Removed

- **`ClipboardBackend::Unimplemented`** dead scaffold variant (and its eight
  `unimplemented!(...)` match arms) — no longer needed now that all supported
  platforms have wired backends.

## [0.4.0] - 2026-05-03

### Breaking

- **`arboard` removed.** The crate no longer depends on `arboard`. All backends
  are hand-rolled. Update your dependency to `hjkl-clipboard = "0.4"`.
- **`Clipboard::new()` now returns `Result<Self, ClipboardError>`** instead of
  `Self`. Callers that used `Clipboard::new()` infallibly must propagate or
  handle the error.
- **`set_text` / `get_text` removed.** Replace with `set` / `get` using
  `MimeType::Text` (see Migration section below).
- **`Selection` enum added** — all clipboard ops now require an explicit
  `Selection::Clipboard` or `Selection::Primary` argument.
- **`MimeType` enum added** — `Text`, `Html`, `Rtf`, `UriList`, `Png`,
  `Custom(String)`.
- **`Uri` type added** — `Uri::File(PathBuf)` and `Uri::Other(String)` for typed
  URI-list handling.
- **`ClipboardError::Io` now wraps `Arc<io::Error>`** (was `io::Error`).
  Pattern-match arms that bound `e: io::Error` must be updated to
  `e: Arc<io::Error>`; dereference with `&*e` to get `&io::Error`.
- **Async API changed signature** — `set_async` / `get_async` / `clear_async` /
  `available_async` are now `pub async fn` returning `impl Future` directly.
  macOS and Windows async variants were `unimplemented!()` in 0.4.0 — fixed in
  0.4.1 to use the native backends (sync-wrapped).

### Added

- **Native Wayland backend** — `ext_data_control_v1` (KDE 6.2+, wlroots
  compositors) + `zwp_primary_selection_v1` for the PRIMARY selection. Hand-
  rolled wire protocol over a raw Unix socket; no libwayland-client dlopen.
- **Native X11 backend** — XCB via `dlopen("libxcb.so.1")`, INCR send and
  receive for payloads beyond the server's `max_request_length`,
  auto-`SAVE_TARGETS` after every successful `set` so clipboard managers always
  have the latest payload.
- **Native macOS backend** — `NSPasteboard` via raw `objc_msgSend`; correct
  calling-convention handling for both x86_64 and ARM64.
- **Native Windows backend** — `CF_UNICODETEXT` (text), registered
  `"HTML Format"` (CF_HTML), `CF_RTF`, registered `"PNG"` + `CF_DIBV5` (PNG ↔
  DIB roundtrip via `miniz_oxide`), `CF_HDROP` (uri-list).
- **OSC 52 fallback** — write-only, SSH + tmux DCS passthrough, capped at ~74
  000 bytes.
- **Rich MIME types** — `Text`, `Html`, `Rtf`, `UriList`, `Png`,
  `Custom(String)`.
- **Typed URI-list helpers** — `set_uri_list` / `get_uri_list` with
  `Uri::File(PathBuf)` / `Uri::Other(String)`, RFC 3986 percent-encode/decode,
  Windows drive-letter and UNC mappings.
- **Sync + async API** — hand-rolled `Future` with `std::task::Waker`, no
  runtime dependency.
- **`Selection::Primary`** — Linux X11 and Wayland support both CLIPBOARD and
  PRIMARY. Other platforms return `UnsupportedMime` for PRIMARY ops.
- **`ClipboardError::clone()`** — `ClipboardError` now derives `Clone` (enabled
  by the `Arc<io::Error>` change above).
- **`ClipboardError::io()` / `ClipboardError::io_other(&str)`** convenience
  constructors.

### Changed

- `Clipboard` is now `Send + Sync`. (The advertised `Clone` impl was missing in
  0.4.0 — fixed in 0.4.1.)
- Backend probe order on Linux: Wayland → X11 → OSC 52. First successful probe
  wins; fallthrough is transparent to the caller.
- `MimeType` and `Selection` are `#[non_exhaustive]` on `MimeType` — adding
  variants in future minor versions will not be a breaking change.

### Removed

- `arboard` dependency.
- `Clipboard::set_text(&str)` — replaced by
  `Clipboard::set(sel, MimeType::Text, bytes)`.
- `Clipboard::get_text()` — replaced by `Clipboard::get(sel, MimeType::Text)`.
- All implicit SSH detection from the public API surface — the backend selector
  handles it internally.

### Migration

**Before (0.3.x):**

```rust
use hjkl_clipboard::Clipboard;

let mut cb = Clipboard::new();          // infallible
cb.set_text("hello");                   // returns bool
let text: Option<String> = cb.get_text();
```

**After (0.4.x):**

```rust
use hjkl_clipboard::{Clipboard, ClipboardError, MimeType, Selection};

let cb = Clipboard::new()?;             // returns Result

// set
cb.set(Selection::Clipboard, MimeType::Text, b"hello")?;

// get — returns Vec<u8>
let bytes = cb.get(Selection::Clipboard, MimeType::Text)?;
let text = String::from_utf8_lossy(&bytes);
```

**`ClipboardError::Io` pattern match:**

```rust
// Before
ClipboardError::Io(e) => { /* e: io::Error */ }

// After
ClipboardError::Io(e) => { /* e: Arc<io::Error> */; let _ = &*e; }
```

## [0.3.1] - 2026-04-30

### Changed

- Migrated `hjkl-clipboard` from the `kryptic-sh/hjkl` monorepo into its own
  repository
  ([kryptic-sh/hjkl-clipboard](https://github.com/kryptic-sh/hjkl-clipboard))
  with full git history preserved.
- Relaxed inter-crate dependency requirements from `=0.3.0` to `0.3` (caret),
  matching the standard SemVer pattern for library dependencies.

### Added

- Standalone `LICENSE`, `.gitignore`, and `ci.yml` workflow at the repo root.
