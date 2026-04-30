# hjkl-clipboard 0.4.0 — Design & Progress

Major rewrite. Drops `arboard` entirely. Hand-rolled per-platform clipboard
implementation with real selection ownership, rich types, sync + async API,
runtime-agnostic.

Tracks progress across the full implementation. Update checkboxes as work lands.

## Why a rewrite

`arboard` silent-fails on Wayland: `set_text` returns `Ok` but the selection
dies when our process exits or when the compositor requires keyboard focus that
a TUI cannot provide. Memory note documents the gap; users have hit it.

We also want rich types (HTML/RTF/PNG/uri-list), explicit selection control
(CLIPBOARD vs PRIMARY), and a sync + async API — none of which arboard exposes
cleanly.

## Scope

- Linux Wayland: `wlr_data_control_v1` + `ext_data_control_v1` +
  `zwp_primary_selection_v1`. GNOME (no data-control) → OSC 52 fallback.
- Linux X11: XCB protocol via `dlopen`, INCR support, auto-`SAVE_TARGETS` after
  every set, CLIPBOARD + PRIMARY selections.
- macOS: NSPasteboard via raw `objc_msgSend` (test x86_64 + ARM64 — calling
  conventions differ).
- Windows: Win32 user32 `OpenClipboard` / `SetClipboardData` / `CF_*` formats.
- OSC 52 fallback: SSH detection, tmux DCS passthrough, write-only.
- Rich types v1: text, HTML (CF_HTML wrap on Win), RTF, uri-list (typed Path
  API), PNG (`miniz_oxide` for DIB↔PNG on Win).

## Out of scope (v2 or later)

- Change listener / `watch()` / clipboard-update events.
- Image formats beyond PNG.
- Wire-format path translation across OSes (clipboards are per-machine; paths
  don't translate).
- Initial-state event on subscribe (no watch in v1 anyway).

## Architecture

- **Hybrid linking**: `#[link]` AppKit/Foundation/libobjc on macOS, `#[link]`
  user32/kernel32 on Windows, `dlopen` libxcb / libwayland-client on Linux.
  Linux earns `dlopen` because of distro/container variety; mac/Win libs always
  present.
- **Singleton bg thread per process** (X11/Wayland), lazy init on first op,
  lives until process exit. Drop of last `Clipboard` handle keeps thread +
  selection alive.
- **`std`-only**, plus two narrow deps:
  - `miniz_oxide` (PNG deflate on Windows DIB↔PNG path).
  - `futures-core` (`Stream` trait — only if we add `watch_async` later; not
    needed in v1).
- **Sync + async API**, hand-rolled `Future`, no runtime dep.
- **Auto-`SAVE_TARGETS`** after every successful X11 `set()` — no `persist()`
  API; clipboard manager always has latest.

## Public API

```rust
pub enum Selection { Clipboard, Primary }

#[non_exhaustive]
pub enum MimeType {
    Text,
    Html,
    Rtf,
    UriList,
    Png,
    Custom(String),  // raw passthrough, no translation
}

pub enum Uri {
    File(PathBuf),
    Other(String),
}

pub enum ClipboardError {
    LibNotFound,        // libxcb/libwayland missing
    NoDisplay,          // no DISPLAY/WAYLAND_DISPLAY/SSH/tty
    PayloadTooLarge,    // OSC 52 cap or platform max
    FocusRequired,      // Wayland without data-control
    UnsupportedMime,    // OSC 52 with non-text mime
    InvalidUri,         // relative path or malformed
    Io(io::Error),
}

pub struct Clipboard { /* ... */ }

impl Clipboard {
    pub fn new() -> Result<Self, ClipboardError>;

    // sync
    pub fn set(&self, sel: Selection, mime: MimeType, bytes: &[u8])
        -> Result<(), ClipboardError>;
    pub fn get(&self, sel: Selection, mime: MimeType)
        -> Result<Vec<u8>, ClipboardError>;
    pub fn clear(&self, sel: Selection) -> Result<(), ClipboardError>;
    pub fn available(&self, sel: Selection)
        -> Result<Vec<MimeType>, ClipboardError>;

    // async (mirror, hand-rolled Future, runtime-agnostic)
    pub fn set_async(&self, sel: Selection, mime: MimeType, bytes: &[u8])
        -> impl Future<Output = Result<(), ClipboardError>>;
    pub fn get_async(&self, sel: Selection, mime: MimeType)
        -> impl Future<Output = Result<Vec<u8>, ClipboardError>>;
    pub fn clear_async(&self, sel: Selection)
        -> impl Future<Output = Result<(), ClipboardError>>;
    pub fn available_async(&self, sel: Selection)
        -> impl Future<Output = Result<Vec<MimeType>, ClipboardError>>;

    // typed uri-list helpers (recommend over raw bytes)
    pub fn set_uri_list(&self, sel: Selection, uris: &[Uri])
        -> Result<(), ClipboardError>;
    pub fn get_uri_list(&self, sel: Selection)
        -> Result<Vec<Uri>, ClipboardError>;
}
```

### URI rules

- Relative paths in `set_uri_list` → `InvalidUri` error. Must be absolute (RFC
  3986 requires).
- Windows UNC paths `\\server\share\foo` ↔ `file://server/share/foo` standard
  mapping, handled internally.
- Symlinks pass through unresolved — caller resolves if needed.
- Non-file URIs (`https://...`) round-trip via `Uri::Other(String)`.
- Cross-OS path strings are passed through verbatim. We don't translate `C:\foo`
  ↔ `/foo`; clipboard is per-machine and the file doesn't exist on the other OS
  anyway.

## Backend specifics

### Windows

- `#[link(name = "user32")]`, `#[link(name = "kernel32")]`.
- Standard formats: `CF_UNICODETEXT`, `CF_HDROP` (uri-list).
- Registered formats: `"HTML Format"` (CF_HTML — needs header wrap),
  `"Rich Text Format"` (CF_RTF), `"PNG"` (modern apps) + `CF_DIBV5` (legacy
  fallback — uses `miniz_oxide` to deflate the IDAT chunks back to a DIB
  bitmap).
- `OpenClipboard(NULL)` (no window owner needed). `EmptyClipboard` on set,
  `GlobalAlloc(GMEM_MOVEABLE)` for payloads, `SetClipboardData` per format.
- No bg thread needed — Windows owns clipboard data after `SetClipboardData`.
- Tests: serialize on shared OS clipboard (`--test-threads=1`).

### macOS

- `#[link(name = "AppKit", kind = "framework")]`,
  `#[link(name = "Foundation", kind = "framework")]`, `#[link(name = "objc")]`.
- `objc_msgSend` calling convention differs x86_64 vs ARM64. Cast function
  pointer to exact signature per call site. Wrong cast = segfault.
- Selectors: `generalPasteboard`, `clearContents`, `setData:forType:`,
  `dataForType:`, `types`, `setString:forType:`, `stringForType:`.
- UTI types: `NSPasteboardTypeString`, `NSPasteboardTypeHTML`,
  `NSPasteboardTypeRTF`, `NSPasteboardTypePNG`, `text/uri-list` for new apps +
  `NSFilenamesPboardType` for legacy.
- No bg thread — NSPasteboard is system-managed.

### Linux X11

- `dlopen("libxcb.so.1")` lazy. Symbols stored in `OnceLock<XcbFns>`.
- Singleton bg thread holds connection + invisible window.
- Atoms: `CLIPBOARD`, `PRIMARY`, `TARGETS`, `UTF8_STRING`, `STRING`,
  `text/plain;charset=utf-8`, `text/html`, `text/rtf`, `text/uri-list`,
  `image/png`, `INCR`, `CLIPBOARD_MANAGER`, `SAVE_TARGETS`, `MULTIPLE`.
- Selection ownership via `XCB_SET_SELECTION_OWNER`.
- Service `XCB_SELECTION_REQUEST` events: target negotiation (`TARGETS` → list
  our atoms; specific target → write data to requestor's property + send
  `XCB_SELECTION_NOTIFY`).
- Service `XCB_SELECTION_CLEAR` events: drop owned data for that selection.
- Read path: `XCB_CONVERT_SELECTION` → wait for `SELECTION_NOTIFY` → read
  property → handle `INCR` for chunked payloads (>~256 KB).
- Auto `SAVE_TARGETS`: after every successful `set()`, send
  `XCB_CONVERT_SELECTION(CLIPBOARD_MANAGER, SAVE_TARGETS, ..)`. Manager grabs
  latest. Idempotent.
- Auth: parse `~/.Xauthority`, MIT-MAGIC-COOKIE-1 in connection setup.

### Linux Wayland

- `dlopen("libwayland-client.so.0")` lazy.
- Singleton bg thread holds connection + roundtrip.
- Bind in priority order: `ext_data_control_v1` → `wlr_data_control_v1` →
  fallback (no data-control = OSC 52 path on writes; reads return
  `FocusRequired`).
- Per-selection: `data_control_manager.create_data_source` +
  `device.set_selection`. Service `send` events (write payload to fd) and
  `cancelled` events (selection lost).
- Primary: same shape on `zwp_primary_selection_v1` + data-control variant if
  compositor exposes it.
- Hardcode the ~6 wire messages we use; skip XML protocol parser.
- Wire protocol: 32-bit aligned messages with fd passing via `SCM_RIGHTS` over
  the unix socket.

### OSC 52 fallback

- Already implemented in current `lib.rs`. Port verbatim.
- Write-only. Text-only (returns `UnsupportedMime` for non-text).
- SSH detect via `SSH_TTY`/`SSH_CONNECTION` env.
- tmux DCS wrap when `$TMUX` is set.
- Used when:
  - Over SSH (any platform).
  - Wayland without data-control protocol (e.g. GNOME).
  - X11 without DISPLAY (rare).

## Async primitive

Hand-rolled, zero new deps. `std::future::Future` + `std::task::Waker` only.

```rust
enum SlotState<T> { Empty, Waiting(Waker), Ready(T), Taken }
struct Oneshot<T> { state: Mutex<SlotState<T>> }
```

Bg thread message has unified reply target:

```rust
enum Reply<T> {
    Sync(Arc<(Mutex<Option<T>>, Condvar)>),
    Async(Arc<Oneshot<T>>),
}
```

Same thread, same protocol, two front doors. ~150 LOC total.

## Testing

### CI matrix

| Job              | Runner         | Setup                                   |
| ---------------- | -------------- | --------------------------------------- |
| `test-pure`      | ubuntu-latest  | no display — base64, mime maps, OSC 52  |
| `test-linux-x11` | ubuntu-latest  | xvfb + mock CLIPBOARD_MANAGER (~50 LOC) |
| `test-linux-wl`  | ubuntu-latest  | sway-headless (`WLR_BACKENDS=headless`) |
| `test-windows`   | windows-latest | `cargo test -- --test-threads=1`        |
| `test-macos-x64` | macos-13       | x86_64 — objc_msgSend ABI               |
| `test-macos-arm` | macos-latest   | ARM64 — objc_msgSend ABI                |

### Manual matrix (per release)

| Compositor / env        | Tests                                |
| ----------------------- | ------------------------------------ |
| Sway, Hyprland, River   | wlr-data-control real compositors    |
| KDE 6.2+                | ext-data-control                     |
| GNOME mutter            | no data-control → OSC 52 fallback    |
| Xorg + klipper / GPaste | real clipboard managers, persistence |
| Xorg without manager    | SAVE_TARGETS fails gracefully        |
| macOS desktop session   | NSPasteboard real apps round-trip    |
| Windows 10 / 11         | clipboard contention with other apps |

Manual checklist in `CONTRIBUTING.md`. Tag each release with results.

## Module layout

```
src/
  lib.rs              # public API surface, re-exports
  error.rs            # ClipboardError
  mime.rs             # MimeType + per-platform name maps
  selection.rs        # Selection enum
  uri.rs              # Uri enum + percent-encode/decode + UNC mapping
  oneshot.rs          # async Oneshot<T> primitive
  reply.rs            # Reply<T> enum (Sync/Async dispatch)
  base64.rs           # extract from current lib.rs
  osc52.rs            # extract from current lib.rs (SSH detect, DCS wrap)
  backend/
    mod.rs            # Backend trait + probe-and-pick
    osc52.rs          # OSC 52 backend (fallback)
    windows.rs        # cfg(windows)
    macos.rs          # cfg(target_os = "macos")
    x11.rs            # cfg(target_os = "linux") — XCB via dlopen
    wayland.rs        # cfg(target_os = "linux") — wire protocol via dlopen
    bg_thread.rs      # singleton thread + message dispatch (linux)
    dlopen.rs         # libxcb / libwayland symbol loaders
    cf_html.rs        # CF_HTML header wrap/unwrap (windows)
    cf_hdrop.rs       # DROPFILES build/parse (windows)
    dib_png.rs        # DIB↔PNG via miniz_oxide (windows)
```

## Implementation phases

Each phase ends with passing tests. Don't merge half-built phases.

### Phase 0 — Scaffold (DONE — `ee00be1`, `b391c82`)

- [x] Bump `Cargo.toml` to 0.4.0, drop `arboard`, add `miniz_oxide`, update
      description.
- [x] Module layout above (empty files, `unimplemented!()` bodies).
- [x] Public API types: `Selection`, `MimeType`, `Uri`, `ClipboardError`,
      `Clipboard` struct.
- [x] Method signatures (sync + async + uri-list helpers), all
      `unimplemented!()`.
- [x] `cargo check` passes on linux. Win/mac targets not installed locally —
      will be verified in CI once Phase 7 sets it up.

Notes from execution:

- `Oneshot::resolve/poll` and `Reply::resolve` got working bodies in Phase 0
  (Phase 1 work landed early). Logic: standard Mutex/Condvar + SlotState enum.
  Acceptable scope creep, no harm.
- `osc52::emit_osc52` refactored to take `in_tmux: bool` as a parameter rather
  than reading the env directly. Cleaner separation between detection
  (`is_in_tmux()`) and emission. Backwards-incompatible with the old private
  signature but the function is `pub(crate)`.
- `#![allow(dead_code)]` at the crate root suppresses scaffold-phase noise.
  Remove once Phase 7 wires everything up.

### Phase 1 — Async primitive + bg thread skeleton

- [ ] `Oneshot<T>` impl + tests.
- [ ] `Reply<T>` enum.
- [ ] `bg_thread.rs` skeleton: spawn-on-demand, accepts `Request { op, reply }`,
      no backend wiring yet.
- [ ] `block_on` integration: sync wraps async via condvar, async wraps Future
      via Oneshot.
- [ ] Tests: enqueue → dummy reply → both sync/async paths return.

### Phase 2 — OSC 52 backend (port from current lib.rs)

- [ ] Move base64 to `base64.rs`, add tests (already present).
- [ ] `osc52.rs`: SSH detect + tmux DCS wrap.
- [ ] `backend/osc52.rs`: implements Backend trait, write-only, text-only.
- [ ] Tests: PTY harness captures stdout, asserts wire format with/without tmux.

### Phase 3 — Windows backend

- [ ] `extern "system"` blocks for user32/kernel32 — compile-checked.
- [ ] `cf_html.rs` header wrap/unwrap + tests (round-trip).
- [ ] `cf_hdrop.rs` DROPFILES build/parse + tests (round-trip + UNC).
- [ ] `dib_png.rs` DIB↔PNG via `miniz_oxide` + tests.
- [ ] Backend impl: open/empty/set/get/clear/available for all mimes.
- [ ] CI green on `test-windows`.

### Phase 4 — macOS backend

- [ ] dlopen + selector cache for objc.
- [ ] x86_64 + ARM64 `objc_msgSend` cast helpers.
- [ ] Backend impl: NSPasteboard for all mimes incl uri-list (both
      `text/uri-list` and `NSFilenamesPboardType`).
- [ ] CI green on both `test-macos-x64` and `test-macos-arm`.

### Phase 5 — Linux X11 backend

- [ ] `dlopen.rs` libxcb loader.
- [ ] XCB connection setup + `~/.Xauthority` parse + MIT-MAGIC-COOKIE-1.
- [ ] Atom interning + cache.
- [ ] Bg thread: invisible window, selection ownership, `SelectionRequest`
      handler with target negotiation.
- [ ] INCR transfer: outgoing chunking + incoming reassembly.
- [ ] Auto-`SAVE_TARGETS` after every successful set.
- [ ] CLIPBOARD + PRIMARY selections.
- [ ] Mock CLIPBOARD_MANAGER for tests (~50 LOC, owns the manager selection,
      responds to SAVE_TARGETS).
- [ ] CI green on `test-linux-x11`.

### Phase 6 — Linux Wayland backend

- [ ] `dlopen.rs` libwayland-client loader.
- [ ] Wire-protocol message marshalling (8 messages: `wl_registry_bind`,
      `wl_seat_get_pointer` no, `data_control_manager.create_data_source`,
      `data_control_manager.get_data_device`, `data_control_source.offer`,
      `data_control_source.send` event, `data_control_device.set_selection`,
      `data_control_offer.receive`, etc.) — finalize during impl.
- [ ] Bind-priority probe: `ext_data_control_v1` → `wlr_data_control_v1` → none
      (mark `FocusRequired`).
- [ ] CLIPBOARD + PRIMARY selections (`zwp_primary_selection_v1` + data-control
      variant).
- [ ] Bg thread: connection alive, fd passing for send events.
- [ ] CI green on `test-linux-wl` (sway-headless).

### Phase 7 — Integration + ship

- [ ] `cargo deny check` clean.
- [ ] `cargo clippy -- -D warnings` clean.
- [ ] `cargo fmt --check` clean.
- [ ] Manual matrix run: sway, hyprland, KDE 6.2+, GNOME, Xorg+klipper,
      Xorg+nothing, macOS, Win 10, Win 11.
- [ ] CHANGELOG entry for 0.4.0 (breaking: removed `set_text`/`get_text`, added
      everything else).
- [ ] README rewrite — new API examples, platform support matrix, GNOME caveat,
      manual SSH paste limitation.
- [ ] BCTP cut: bump 0.3.1 → 0.4.0 in own repo, tag, push.
- [ ] hjkl umbrella update: bump `hjkl-clipboard = "0.4"` in
      `apps/hjkl/Cargo.toml`, update host adapter.

## Open items

- Wayland wire-message list — finalize during phase 6 impl, exact set depends on
  which data-control variants we end up supporting.
- macOS UTIs — confirm `NSPasteboardTypeURL` vs `text/uri-list` precedence
  during phase 4.
- Mock CLIPBOARD_MANAGER implementation detail — write during phase 5.

## Notes for future maintainers

- `MimeType` is `#[non_exhaustive]` — adding variants in v2+ is not a breaking
  change. Use `Custom(String)` for v1 escape hatch.
- Drop semantics: bg thread + selection live until process exit. This is
  intentional, matches arboard / Helix / every clipboard lib. Don't switch to
  RAII teardown without strong justification.
- v2 candidates (in priority order): `watch()` listener (~250 LOC, +1 week,
  futures-core dep), additional image formats (JPEG/WebP/SVG), wire-format trace
  logging for debugging compositor variance.
