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

### Phase 1 — Async primitive + bg thread skeleton (DONE — `4e9507f`)

- [x] `Oneshot<T>` impl + tests (6 tests: resolve-before-poll,
      poll-before-resolve, multi-poll, panic-on-take, concurrent cross-thread
      with UnparkWaker, drop).
- [x] `Reply<T>` enum + 3 tests (Sync condvar delivery, Async oneshot
      forwarding, Send-safety).
- [x] `bg_thread.rs` skeleton: lazy `OnceLock<BgThread>` singleton, mpsc inbox,
      `Op::Echo` test op, `dispatch()` function.
- [x] Sync + async send paths: `send_sync` blocks on condvar; `send_async`
      returns `OneshotFuture` (named type, `pub(crate)`).
- [x] Tests: 4 bg_thread roundtrip tests (sync, async via park-loop block_on,
      sequential, 10-thread concurrent burst).

Notes from execution:

- `Request` payload is monomorphic (`Reply<Result<String, ClipboardError>>`) for
  Phase 1. Generic-over-ops will come when Phase 5/6 introduce
  `Set`/`Get`/`Clear`/`Available` with different reply types — likely via per-op
  channels or a payload enum.
- Public `Clipboard::*` methods deliberately untouched — Phase 1 is pure
  plumbing. Phase 2+ wires the backends in.
- `block_on` test helper is a tiny park-loop with `UnparkWaker`; the bg thread's
  `waker.wake()` triggers `thread::unpark` so the loop re-polls immediately.
  Zero dep cost.

Minor test weaknesses (acceptable, won't block downstream phases):

- `drop_without_resolve` test only covers `new; drop`. Doesn't exercise
  drop-after-resolve-unread or drop-after-poll-pending. Std
  `Mutex<SlotState<T>>` handles these correctly; just under-tested.
- `multiple_polls_before_resolve` doesn't assert the OLD waker is not fired when
  a NEW waker overwrites it. Behavior is correct (latest-wins); test
  under-verifies.

### Phase 2 — OSC 52 backend (DONE — `f3f6910`, dedup follow-up)

- [x] Move base64 to `base64.rs` (done in Phase 0).
- [x] `osc52.rs`: SSH detect + tmux DCS wrap (done in Phase 0). Phase 2 split
      into testable `write_osc52(&mut impl Write, ...)` + stdout convenience
      wrapper `emit_osc52`.
- [x] `backend/osc52.rs`: implements `Backend`. Text-only, write-only,
      `Selection::Clipboard`-only. Other mimes / Primary / non-UTF-8 →
      `UnsupportedMime`. Oversize via `OSC52_MAX` → `PayloadTooLarge`. `clear`
      emits empty OSC 52. `available` returns `Ok(vec![])` (cannot read terminal
      clipboard).
- [x] Wire-format tests in `osc52::tests` (8 tests with inline `base64_decode`
      helper, no dep). Trait dispatch tests in `backend/osc52::tests` (14
      tests).
- [x] Trait impls delegate to `set_inner`/`clear_inner` with
      `io::stdout().lock()` — single source of truth, no duplicated validation
      logic.

Notes from execution:

- Test count: 15 → 37 (+22).
- `Backend::available` returns `Ok(vec![])` for both Clipboard and Primary
  (deviation from prompt suggestion that Primary should error). Justification:
  `available` semantics are "what's there to read"; an empty list communicates
  "nothing readable" without making the caller handle a special error case for
  an unsupported selection.
- `emit_osc52` (the stdout convenience) is currently dead code — trait impls
  call `set_inner` directly with `io::stdout().lock()`. Kept for now under
  crate-wide `#![allow(dead_code)]`. Phase 7 dead code audit will decide whether
  to keep or drop.

### Phase 3 — Windows backend (split into sub-phases)

Split because Phase 3 is ~900 LOC across four interrelated wire formats, with no
Windows runner locally — only `cargo check --target x86_64-pc-windows-gnu`
verifies type-correctness. Smaller chunks per audit = more confidence per
landing.

#### Phase 3a — Win32 FFI + text + clear (DONE — `9abfc56`, LockedHandle follow-up)

- [x] `unsafe extern "system"` blocks for user32 (OpenClipboard, CloseClipboard,
      EmptyClipboard, GetClipboardData, SetClipboardData,
      IsClipboardFormatAvailable, EnumClipboardFormats) and kernel32
      (GlobalAlloc, GlobalLock, GlobalUnlock, GlobalFree, GlobalSize,
      GetLastError) — no winapi/windows-sys dep.
- [x] Type aliases (BOOL/DWORD/UINT/SIZE_T/HWND/HANDLE/HGLOBAL) in a
      `mod win32_types` with narrow
      `#[allow(clippy::upper_case_acronyms,     non_camel_case_types)]`.
- [x] `ClipboardOpen` RAII guard pairs OpenClipboard/CloseClipboard.
- [x] `LockedHandle` RAII guard pairs GlobalLock/GlobalUnlock — landed in
      follow-up to fix lock-leak on UTF-16 decode error in `get_text`. Same
      primitive will be reused by 3b/3c/3d.
- [x] `set_text` / `get_text` for `CF_UNICODETEXT` (UTF-8 ↔ UTF-16 LE + null
      terminator).
- [x] `WindowsBackend` implements `Backend`. Text routes to helpers; other mimes
      return `UnsupportedMime` (filled in by 3b/3c/3d). `clear` calls
      `EmptyClipboard`. `available` enumerates via `EnumClipboardFormats`, maps
      `CF_UNICODETEXT` → `MimeType::Text`.
- [x] `Selection::Primary` returns `UnsupportedMime` for set/get/clear,
      `Ok(vec![])` for available — Windows has no primary selection; consistent
      with OSC 52 backend convention.
- [x] `cargo check --target x86_64-pc-windows-gnu` passes clean.
- [x] `cargo clippy --target x86_64-pc-windows-gnu` clean.
- [x] All `unsafe` blocks have SAFETY comments.

Notes from execution:

- Edition 2024 mandates `unsafe extern "system"` for raw FFI blocks.
- `bg_thread` and `dlopen` modules also gated `cfg(target_os = "linux")` —
  agent's reasonable extension since they're Linux-only by design.
- `cf_hdrop`/`cf_html`/`dib_png` modules gated `cfg(target_os = "windows")`
  since they're Windows-internal helpers.
- Linux native build has zero Win32 code compiled — clean separation.
- 37 tests still passing on Linux (no test changes — Windows tests deferred to
  Phase 7 CI).

#### Phase 3b — CF_HTML + CF_RTF (DONE — `21b663e`)

- [x] `cf_html.rs` moved from `src/backend/` to `src/` (non-cfg-gated) so the
      pure-rust wrap/unwrap is testable on Linux native CI. `cf_hdrop.rs` and
      `dib_png.rs` also pre-emptively moved for the same reason; 3c/3d will fill
      them in.
- [x] `cf_html::wrap` builds the MS envelope with a fixed-length 128-byte header
      (zero-padded `{:010}` decimal offsets) + `BODY_OPEN` / `BODY_CLOSE`
      wrappers. `debug_assert_eq!` guards against template drift.
- [x] `cf_html::unwrap` parses header until first `<` byte, validates
      `StartFragment` / `EndFragment` (presence + numeric + bounds + ordering +
      UTF-8). Malformations →
      `ClipboardError::Io(other("malformed CF_HTML     header"))`.
- [x] `RegisterClipboardFormatW` added to user32 FFI.
- [x] `cf_html_format()` / `cf_rtf_format()` cache registered IDs via
      `OnceLock<UINT>`. 0-return propagated as `ClipboardError::Io` at call
      sites — no panic, no `Option` ceremony.
- [x] `set_bytes` / `get_bytes` generic byte-exact helpers over registered
      formats. Reuse `LockedHandle` from 3a. Text path keeps its own
      UTF-16-converting helpers — clean separation.
- [x] `WindowsBackend::set/get/available` wires Html + Rtf paths. UriList / Png
      / Custom remain `UnsupportedMime` (filled in by 3c/3d).
- [x] `cargo check` / `clippy` clean on both linux + windows-gnu.
- [x] 50 tests passing (37 prior + 13 new cf_html round-trip + edge cases).

Notes from execution:

- "Swapped-offsets" test only asserts the bounds check fires; the inner content
  under swapped offsets is still valid UTF-8, so the
  `StartFragment > EndFragment` branch is what catches it. Indirect but the
  intended check fires.
- `unwrap` falls back to scanning the entire payload if no `<` byte is found.
  Edge case but reasonable.
- Header length integrity in release builds depends on `debug_assert_eq!`
  catching typos at test time. `{:010}` always produces exactly 10 chars, so the
  assertion only fires on template-string typos — real risk is low given how
  localized the constants are.

#### Phase 3c — CF_HDROP + uri-list helpers (DONE — `74bc83d`)

- [x] `cf_hdrop::build` / `cf_hdrop::parse` for DROPFILES (20-byte header:
      pFiles=20, pt.x/y, fNC=0, fWide=1, all i32/u32 LE) + UTF-16 LE paths +
      double-null terminator. Pure rust, Linux-testable.
- [x] `uri::encode_uri_list` / `decode_uri_list` (RFC 2483) + `path_to_file_uri`
      / `file_uri_to_path` with `cfg!(windows)` branching for drive-letter
      (`file:///C:/...`) and UNC (`file://server/share/...`) mappings.
- [x] Inline percent-encode/decode (RFC 3986 unreserved set, plus `/` and `:`
      kept bare for path/scheme readability). No new dep.
- [x] `Clipboard::set_uri_list` / `get_uri_list` typed helpers wired — encode
      validates relative paths up-front (`InvalidUri`).
- [x] Windows backend: text/uri-list ↔ CF_HDROP round-trip in `set_uri_list` /
      `get_uri_list` helpers. `WindowsBackend::set/get/available` wires the
      UriList path. `CF_HDROP = 15` standard constant.
- [x] 31 new tests (14 cf_hdrop + 22 uri, scoped via `cfg(windows)` /
      `cfg(not(windows))` so Linux runs Unix cases and Windows runs Win cases).
- [x] `cargo check` / `clippy` clean on linux + windows-gnu.
- [x] 81 tests passing on Linux native (50 prior + 31 new).

Notes from execution:

- Layering: internal canonical wire format is **text/uri-list bytes**. Backend
  stays bytes-only. Windows pays a small encode/decode round-trip cost
  (text/uri-list → paths → CF_HDROP and reverse). Negligible at clipboard scale;
  v2 can specialize via Backend trait additions if perf matters.
- `Uri` gained `PartialEq + Eq` derives — needed for round-trip
  `assert_eq!(uris, decoded)` tests. Harmless.
- `is_unreserved` includes `/` and `:` beyond the strict RFC 3986 unreserved
  set. Standard for file URIs (path separators stay readable; drive-letter
  colons stay bare).
- `cf_hdrop::build` does not validate Windows path shape (drive letter / UNC).
  The Windows backend calls it; shape validity is enforced upstream via
  `path_to_file_uri`. Build only validates: non-empty + no interior nulls.
- MAX_PATH (260) not enforced — Windows 10+ apps can opt into long paths via
  `LongPathsEnabled`. Truncating or erroring would be wrong silently.
- Path-string round-trip on Linux uses `PathBuf::from(string)` which works fine
  for testing the wire format but doesn't exercise real Windows path semantics.
  Phase 7 Windows CI catches that.

#### Phase 3d — DIB ↔ PNG (DONE — `554d33e`)

- [x] `dib_png.rs` PNG chunk framing (IHDR/IDAT/IEND) + deflate/inflate via
      `miniz_oxide` + DIBV5 header build/parse. Pure-rust IEEE 802.3 CRC32 with
      `OnceLock` table cache. All 5 PNG filters implemented for unfilter; emits
      filter type 0 (None) when building.
- [x] Registered `"PNG"` format passthrough for modern apps + `CF_DIBV5`
      (`UINT = 17`) fallback for legacy. `cf_png_format()` helper mirrors
      `cf_html_format()` / `cf_rtf_format()` shape.
- [x] DIBV5 header: 124 bytes; 32 bpp uses `BI_BITFIELDS` with explicit ARGB
      masks so apps interpret alpha; 24 bpp uses `BI_RGB`. Bottom-up emit
      (positive height); both bottom-up and top-down accepted on parse. Row
      stride padded to 4-byte boundary. Channel order BGR(A) on the wire,
      converted from PNG RGB(A).
- [x] `set_png` opens clipboard once, `EmptyClipboard`, then sets both `"PNG"`
      and `CF_DIBV5` in one open. `png_to_dib` runs before opening clipboard so
      conversion failure errors immediately without touching clipboard state.
- [x] `get_png` prefers `"PNG"` passthrough, falls back to `CF_DIBV5` →
      `dib_to_png` conversion. Returns `UnsupportedMime` if neither present.
- [x] `available` reports `MimeType::Png` exactly once whether `"PNG"`,
      `CF_DIBV5`, or both formats are enumerated (dedup via `png_seen` flag).
- [x] 11 round-trip + edge tests (RGBA/RGB 2x2, single row, 3x2 RGB stride
      padding, top-down DIB parse, bad signature, header size mismatch, palette
      PNG, 16-bit PNG, 124-byte header verification, CRC32 known vector).
- [x] `cargo check` / `clippy` clean on both linux + windows-gnu.
- [x] 92 tests passing on Linux native (81 prior + 11 new).
- [ ] CI green on `test-windows` — deferred to Phase 7.

Notes from execution:

- `set_png` partial-failure: if the first `SetClipboardData("PNG")` succeeds but
  the second `SetClipboardData(CF_DIBV5)` fails, the clipboard ends up with only
  the PNG format (no DIB). Acceptable — modern apps still find PNG. Strict
  all-or-nothing would require staging both handles before any
  `SetClipboardData` call.
- Channel masks for 24 bpp BI_RGB are zero-filled (masks meaningless without
  BI_BITFIELDS). Header is still 124 bytes regardless.
- PNG palette / 16-bit unsupported tests rely on the format match running before
  inflate — confirmed by reading order in `png_to_dib`.

### Phase 4 — macOS backend (DONE — `b3cdb3f`)

- [x] `#[link]` AppKit + Foundation frameworks (no dlopen — mac libs always
      present per architecture decision). `#[link(name = "objc")]` for
      `sel_registerName` / `objc_getClass` / `objc_msgSend`.
- [x] Selector + class cache via `OnceLock<usize>` (raw pointers aren't `Send`;
      cast back at use site). `sel_cached!` and `class_cached!` macros for
      ergonomics. 12 selectors + 3 classes cached.
- [x] `objc_msgSend` declared as zero-sig stub; `msg0`/`msg1`/`msg2` helpers
      transmute it per call-site to the exact `(Id, Sel, args...) -> R`
      signature. Same machine code on both x86_64 and ARM64 because all our
      arguments are pointer/usize-sized (no float/SIMD/large-struct returns).
- [x] `nsstring_from_str` / `nsstring_to_string` / `nsdata_from_bytes` /
      `nsdata_to_vec` helpers — copy bytes into Rust ownership immediately to
      avoid autorelease-pool drain hazards.
- [x] `mime_to_uti` / `uti_to_mime` mapping: `public.utf8-plain-text` (+ legacy
      `NSStringPboardType` accepted), `public.html`, `public.rtf`,
      `text/uri-list`, `public.png`. `Custom(s)` passes through verbatim on
      `set`/`get`; unknown UTIs filtered from `available()` to avoid noise.
- [x] `MimeType` got `PartialEq` derive — needed for `out.contains(&mime)` dedup
      in `available`. Already harmless across the rest of the crate.
- [x] `Selection::Primary`: `set/get/clear` return `UnsupportedMime`,
      `available` returns `Ok(vec![])` — consistent with Windows backend
      convention.
- [x] All `unsafe` blocks have SAFETY comments. `general_pasteboard()` nil-check
      defensive (returns `Io` error rather than crashing).
- [x] Cross-compile clean on `aarch64-apple-darwin`, `x86_64-apple-darwin`,
      `x86_64-pc-windows-gnu`, linux native. Clippy `-D warnings` clean on all
      four.
- [x] 92 tests still passing on Linux native (no new tests — backend requires
      live NSPasteboard at runtime; covered by Phase 7 CI).
- [ ] CI green on `test-macos-x64` and `test-macos-arm` — deferred to Phase 7.

Notes from execution:

- **Deviation from spec**: `NSFilenamesPboardType` (deprecated since macOS
  10.14, requires `NSArray<NSString*>` — extra ABI surface) is **not** written
  on `set_uri_list`. Only `text/uri-list` raw bytes go on the pasteboard. Modern
  Mac apps that consume cross-platform clipboards expect `text/uri-list`;
  legacy-only consumers will not see file URIs from this lib. Acceptable
  trade-off for declining benefit.
- `setData:forType:` returns Apple `BOOL` (signed char). Agent typed it as Rust
  `bool` via transmute. Apple guarantees `YES=1`/`NO=0`, so byte values are
  always valid `bool` patterns — correct in practice. Strictly canonical pattern
  is `let ok: i8 = msg2(...); if ok == 0 { ... }`. Defer fix unless a future
  Apple SDK change ever returns non-`{0,1}` BOOL (extremely unlikely).
- `clearContents` correctly typed as `isize` (NSInteger change count, signed).
- Edition 2024 `if let ... && ...` chain used in `available` for dedup — clippy
  required collapse from nested `if`.
- No autorelease pool management: NSPasteboard objects are autoreleased into the
  caller's pool. We copy bytes out via `nsdata_to_vec` and `nsstring_to_string`
  before any drain can happen. For a long-running TUI process the system
  autorelease pool is fine; if a user calls our API from a thread without a
  pool, autoreleased objects will leak until thread exit. Phase 7 CI on real
  macOS will confirm.

### Phase 5 — Linux X11 backend (split into sub-phases)

Total est. ~1250 LOC across four interrelated wire-protocol layers. We have a
real Linux runner (xvfb) so each piece can land + test individually. Split is
for review-ability, not confidence.

#### Phase 5a — dlopen + connection + auth + atoms (DONE — `78219f4`)

- [x] `src/backend/dlopen.rs` real impl: load `libxcb.so.1` via `libc::dlopen` /
      `dlsym`, store all 5a-5d fn pointers in one `XcbFns` struct cached in
      `OnceLock<Option<XcbFns>>`. `LibNotFound` on missing.
- [x] Symbol set (finalized): `xcb_connect`, `xcb_disconnect`,
      `xcb_connection_has_error`, `xcb_get_setup`, `xcb_setup_roots_iterator`,
      `xcb_intern_atom{,_reply}`, `xcb_flush`, `xcb_generate_id` (5a) +
      `xcb_create_window`, `xcb_set_selection_owner`,
      `xcb_get_selection_owner{,_reply}`, `xcb_change_property`,
      `xcb_send_event`, `xcb_wait_for_event`, `xcb_poll_for_event`,
      `xcb_request_check` (5b) + `xcb_convert_selection`,
      `xcb_get_property{,_reply,_value,_value_length}`, `xcb_delete_property`
      (5c). 24 symbols total — single load on first use.
- [x] Connection: `xcb_connect(NULL, NULL)` — XCB itself parses `$DISPLAY` and
      reads `~/.Xauthority` (MIT-MAGIC-COOKIE-1 included). **No hand-rolled
      xauth parser needed.** This simplifies 5a substantially vs the original
      plan.
- [x] Connection handshake: handled internally by `xcb_connect`. We read screen
      info from `xcb_get_setup` + `xcb_setup_roots_iterator` (16-byte struct
      returned by value — fits in two registers on x86_64/aarch64 ABI).
- [x] Manual offset reads of `xcb_screen_t` fields (root@0, width@20, height@22,
      root_visual@32) and `xcb_setup_t::maximum_request_length` at offset 26 —
      XCB ABI is stable; layout matches libxcb generated bindings.
      `max_request_length` is in 4-byte units in the wire protocol; we multiply
      by 4 for byte length.
- [x] Atom interning: 14 atoms batched (all `xcb_intern_atom` requests sent
      first, then all replies collected — XCB pipelines this). Each reply
      `libc::free`d after extracting the atom value.
- [x] `X11Connection` struct holds `fns + conn + screen + atoms`. Manual
      `unsafe impl Send` (must hand off to bg thread once); deliberately not
      `Sync`. `Drop` calls `xcb_disconnect`.
- [x] `X11Backend` Backend impl stays `unimplemented!()` — wired in 5b/5c/5d.
- [x] `Cargo.toml`: `libc = "0.2"` added under
      `[target.'cfg(target_os = "linux")'.dependencies]`.
- [x] Tests: `dlopen_smoke` (libxcb resolves all symbols),
      `xvfb_connection_and_atoms` (spawn `Xvfb :99 -screen 0 800x600x24 -ac`,
      poll Unix socket connectability for 5s, set `DISPLAY=:99`,
      `X11Connection::open()`, assert screen dims = 800x600 + all 14 atoms
      non-zero, restore `DISPLAY`, drop guard kills Xvfb). 92 → 95 tests.
- [x] All cross-targets clean (linux + windows-gnu + arm64-darwin + clippy
      `-D warnings` on each).

Notes from execution:

- **Major simplification**: `xcb_connect(NULL, NULL)` handles `$DISPLAY` parse
  - `~/.Xauthority` lookup + MIT-MAGIC-COOKIE-1 internally. Saves ~150 LOC of
    hand-rolled auth/handshake code. Real-desktop xauth confirmation deferred to
    Phase 7 manual matrix (xvfb test uses `-ac` to skip auth).
- Xvfb test uses `-ac` (disable host access control) instead of generating an
  Xauthority cookie. Dev box has no `~/.Xauthority`; `-ac` is simpler.
- Readiness probe uses `UnixStream::connect("/tmp/.X11-unix/X99")`, not just
  file-existence check — the socket file appears before Xvfb is ready to accept
  connections, and connect-probe eliminates the race under parallel
  `cargo test`.
- `std::env::set_var` is `unsafe` in Edition 2024 (env mutation is process-
  global). Test mutates `DISPLAY` briefly; only test that touches `DISPLAY` so
  no real concurrent-access UB. SAFETY comments document the assumption.
- `dlopen` handle is intentionally leaked (lives until process exit). Matches
  pattern of macOS framework linking — singleton lib, no unload.
- Symbols for 5b/5c/5d included in `XcbFns` upfront — unused fn pointers do no
  harm, simpler than amending the struct each phase.

#### Phase 5b — bg thread + ownership + small-payload write/TARGETS (TODO, ~400 LOC)

- [ ] Singleton bg thread (extend `bg_thread.rs` or sibling `x11_thread.rs` —
      decide during impl) holds connection + invisible override-redirect window.
- [ ] `Op::Set { sel, mime, bytes }`: store in
      `HashMap<(Selection, MimeAtom), Vec<u8>>`, call `xcb_set_selection_owner`.
- [ ] `Op::Clear { sel }`: drop entries, set owner = NONE.
- [ ] Event loop:
  - `SELECTION_REQUEST` for `TARGETS` → reply with atom list of owned mimes.
  - `SELECTION_REQUEST` for specific target → write payload to requestor's
    property, send `SELECTION_NOTIFY`.
  - `SELECTION_CLEAR` → drop owned data for that selection.
- [ ] No INCR yet — payloads exceeding `max_request_length` return
      `PayloadTooLarge` (5d will lift this).
- [ ] CLIPBOARD + PRIMARY selections wired.
- [ ] Test: round-trip text via xvfb on both selections.

#### Phase 5c — read path + INCR receive + available (TODO, ~250 LOC)

- [ ] `Op::Get { sel, mime }`: `xcb_convert_selection` to private property →
      wait for `SELECTION_NOTIFY` → read property → return bytes.
- [ ] INCR receive: if property type is `INCR`, subscribe to `PROPERTY_NOTIFY`
      events on the requestor window, delete property to ack, accumulate chunks
      until zero-length property arrives.
- [ ] `Op::Available { sel }`: convert `TARGETS`, parse atom array, map atoms
      back to `MimeType` (drop unknown).
- [ ] Tests: read text written by another xvfb client; read large payload via
      INCR.

#### Phase 5d — INCR send + SAVE_TARGETS + mock manager + ship (TODO, ~250 LOC)

- [ ] INCR send: when reply exceeds `max_request_length`, write `INCR` atom +
      total size to property, then chunk via `PROPERTY_NOTIFY` ack loop.
- [ ] Auto-`SAVE_TARGETS` after every successful `set()`. Check
      `xcb_get_selection_owner(CLIPBOARD_MANAGER)` first; skip if no owner
      (otherwise we hang on a `SELECTION_NOTIFY` that never comes).
- [ ] Mock `CLIPBOARD_MANAGER` (~50 LOC test harness): owns the manager
      selection, accepts `SAVE_TARGETS` requests, asserts received targets.
- [ ] Tests: small + large INCR round-trip, persistence with mock manager,
      persistence absence (no manager → graceful no-op).
- [ ] CI green on `test-linux-x11`.

##### Notes / risk

- Worst risk is bg-thread + event-loop architecture in 5b. Once that shape's
  right, 5c/5d are mechanical extensions.
- Auto-`SAVE_TARGETS` in 5d depends on a manager existing —
  `xcb_get_selection_owner` check before firing, otherwise the
  `SELECTION_NOTIFY` wait hangs.
- Test infra: `xvfb-run` (Arch: `xorg-server-xvfb`). Per-test or session-scoped
  decided during 5b.

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
