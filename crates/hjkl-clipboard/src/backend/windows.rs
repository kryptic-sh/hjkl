//! Windows clipboard backend via Win32 user32/kernel32.
//!
//! Uses `OpenClipboard(NULL)`, `EmptyClipboard`, `GlobalAlloc(GMEM_MOVEABLE)`,
//! and `SetClipboardData` / `GetClipboardData` for all supported formats.
//!
//! No `winapi` or `windows-sys` dependency — raw `extern "system"` FFI only.

use std::ffi::c_void;
use std::sync::OnceLock;

use crate::{ClipboardError, MimeType, Selection, Uri};

use super::Backend;

// ---------------------------------------------------------------------------
// Win32 type aliases — integer-backed, no external crates.
// ---------------------------------------------------------------------------

// Win32 conventional names are all-caps; suppress the clippy lint that wants
// camel-case acronyms. The names are intentionally mirroring the C API so
// that diffs against MSDN documentation are easy to read.
#[allow(clippy::upper_case_acronyms, non_camel_case_types, dead_code)]
mod win32_types {
    use std::ffi::c_void;

    pub(super) type BOOL = i32;
    pub(super) type DWORD = u32;
    pub(super) type UINT = u32;
    pub(super) type SIZE_T = usize;
    pub(super) type HWND = *mut c_void;
    pub(super) type HANDLE = *mut c_void;
    pub(super) type HGLOBAL = HANDLE;
}

use win32_types::{BOOL, DWORD, HANDLE, HGLOBAL, HWND, SIZE_T, UINT};

// ---------------------------------------------------------------------------
// Win32 constants.
// ---------------------------------------------------------------------------

/// Clipboard format: Unicode text (UTF-16 LE, null-terminated).
const CF_UNICODETEXT: UINT = 13;

/// Clipboard format: DROPFILES structure (file drag/drop — uri-list).
const CF_HDROP: UINT = 15;

/// Clipboard format: `CF_DIBV5` — device-independent bitmap V5 (legacy PNG fallback).
const CF_DIBV5: UINT = 17;

/// `GlobalAlloc` flag: allocate moveable (required for clipboard data).
const GMEM_MOVEABLE: UINT = 0x0002;

// ---------------------------------------------------------------------------
// FFI — user32: clipboard operations.
// ---------------------------------------------------------------------------

#[link(name = "user32")]
unsafe extern "system" {
    /// Opens the clipboard and associates it with the calling thread.
    ///
    /// Pass `NULL` for `hwnd` to open without a window owner, which is the
    /// standard approach for console / library code.
    fn OpenClipboard(hwnd: HWND) -> BOOL;

    /// Closes the clipboard. Must be paired with every successful `OpenClipboard`.
    fn CloseClipboard() -> BOOL;

    /// Empties the clipboard and frees handles owned by it. Must be called while
    /// the clipboard is open (after `OpenClipboard`).
    fn EmptyClipboard() -> BOOL;

    /// Retrieves data from the clipboard in the specified format. The returned
    /// handle is owned by the clipboard — **do NOT free it**.
    fn GetClipboardData(format: UINT) -> HANDLE;

    /// Places data on the clipboard in the specified format. After a successful
    /// call the clipboard **owns** the handle — do NOT free it. On failure the
    /// caller must free the handle.
    fn SetClipboardData(format: UINT, hMem: HANDLE) -> HANDLE;

    /// Checks whether the clipboard contains data in the given format.
    /// Returns non-zero if available.
    fn IsClipboardFormatAvailable(format: UINT) -> BOOL;

    /// Enumerates available clipboard formats. Pass `0` to start; each call
    /// returns the next format, or `0` at the end (or on error).
    fn EnumClipboardFormats(format: UINT) -> UINT;

    /// Registers a new clipboard format. Returns a format ID > 0 on success, 0
    /// on failure. If the same name is registered by multiple processes, all
    /// receive the same ID (stable for the OS session).
    fn RegisterClipboardFormatW(lpszFormat: *const u16) -> UINT;
}

// ---------------------------------------------------------------------------
// FFI — kernel32: global memory + error retrieval.
// ---------------------------------------------------------------------------

#[link(name = "kernel32")]
unsafe extern "system" {
    /// Allocates the specified number of bytes from the heap.
    fn GlobalAlloc(uFlags: UINT, dwBytes: SIZE_T) -> HGLOBAL;

    /// Locks a global memory object and returns a pointer to the first byte.
    /// Returns `NULL` on failure.
    fn GlobalLock(hMem: HGLOBAL) -> *mut c_void;

    /// Decrements the lock count of the memory object. The return value
    /// indicates whether the memory is still locked; it is generally ignored
    /// after clipboard use.
    fn GlobalUnlock(hMem: HGLOBAL) -> BOOL;

    /// Frees the specified global memory object. Returns `NULL` on success,
    /// the original handle on failure.
    fn GlobalFree(hMem: HGLOBAL) -> HGLOBAL;

    /// Returns the size, in bytes, of the specified global memory object.
    fn GlobalSize(hMem: HGLOBAL) -> SIZE_T;

    /// Retrieves the calling thread's last-error code value.
    // Loaded for completeness; not yet called in this version.
    #[allow(dead_code)]
    fn GetLastError() -> DWORD;
}

// ---------------------------------------------------------------------------
// Registered clipboard format IDs.
// ---------------------------------------------------------------------------
//
// CF_HTML and CF_RTF are not predefined numeric constants — each process must
// call `RegisterClipboardFormatW` to obtain a session-stable ID. We cache the
// result in a `OnceLock<UINT>` so we call the API at most once per process.
// A return value of 0 indicates registration failure (extremely rare; would
// require kernel resource exhaustion). We propagate 0 as a sentinel and let
// callers return an error.

/// Returns the registered format ID for `"HTML Format"` (CF_HTML).
fn cf_html_format() -> UINT {
    static ID: OnceLock<UINT> = OnceLock::new();
    *ID.get_or_init(|| {
        let name: Vec<u16> = "HTML Format\0".encode_utf16().collect();
        // SAFETY: `name` is a valid null-terminated UTF-16 string with a `\0`
        // code unit at the end. `RegisterClipboardFormatW` reads only up to
        // the null terminator and is safe to call from any thread.
        unsafe { RegisterClipboardFormatW(name.as_ptr()) }
    })
}

/// Returns the registered format ID for `"PNG"` (modern PNG passthrough).
fn cf_png_format() -> UINT {
    static ID: OnceLock<UINT> = OnceLock::new();
    *ID.get_or_init(|| {
        let name: Vec<u16> = "PNG\0".encode_utf16().collect();
        // SAFETY: same as `cf_html_format` above.
        unsafe { RegisterClipboardFormatW(name.as_ptr()) }
    })
}

/// Returns the registered format ID for `"Rich Text Format"` (CF_RTF).
fn cf_rtf_format() -> UINT {
    static ID: OnceLock<UINT> = OnceLock::new();
    *ID.get_or_init(|| {
        let name: Vec<u16> = "Rich Text Format\0".encode_utf16().collect();
        // SAFETY: same as `cf_html_format` above.
        unsafe { RegisterClipboardFormatW(name.as_ptr()) }
    })
}

// ---------------------------------------------------------------------------
// RAII clipboard guard.
// ---------------------------------------------------------------------------

/// Holds the clipboard open for the lifetime of this guard.
///
/// `OpenClipboard` is called on construction; `CloseClipboard` is called on
/// drop. This guarantees the clipboard is always closed, even on early return
/// or panic.
struct ClipboardOpen;

impl ClipboardOpen {
    fn new() -> Result<Self, ClipboardError> {
        // SAFETY: OpenClipboard with a NULL hwnd is the documented way to
        // acquire the clipboard from code that does not own a window. The call
        // is safe to make from any thread; failure is indicated by a zero
        // return value.
        let ok = unsafe { OpenClipboard(std::ptr::null_mut()) };
        if ok == 0 {
            return Err(ClipboardError::io_other("OpenClipboard failed"));
        }
        Ok(Self)
    }
}

impl Drop for ClipboardOpen {
    fn drop(&mut self) {
        // SAFETY: this Drop is only reached when `new()` returned `Ok`, which
        // means `OpenClipboard` succeeded. Every successful `OpenClipboard`
        // must be paired with exactly one `CloseClipboard`.
        unsafe {
            CloseClipboard();
        }
    }
}

/// Holds a `GlobalLock` for the lifetime of this guard.
///
/// `GlobalLock` is called on construction; `GlobalUnlock` is called on drop.
/// This guarantees the lock count is decremented even on early return / panic.
struct LockedHandle {
    handle: HGLOBAL,
    ptr: *mut c_void,
}

impl LockedHandle {
    fn new(handle: HGLOBAL) -> Result<Self, ClipboardError> {
        // SAFETY: `handle` is a caller-supplied HGLOBAL. `GlobalLock` is safe
        // to call on any valid HGLOBAL; failure is indicated by a NULL return.
        let ptr = unsafe { GlobalLock(handle) };
        if ptr.is_null() {
            return Err(ClipboardError::io_other("GlobalLock failed"));
        }
        Ok(Self { handle, ptr })
    }

    fn ptr(&self) -> *mut c_void {
        self.ptr
    }
}

impl Drop for LockedHandle {
    fn drop(&mut self) {
        // SAFETY: this Drop is only reached when `new()` returned `Ok`, which
        // means `GlobalLock` succeeded. The lock count must be decremented.
        unsafe {
            GlobalUnlock(self.handle);
        }
    }
}

/// A `GMEM_MOVEABLE` block holding a copy of some bytes — the only kind of
/// memory `SetClipboardData` accepts. Owned until [`replace_clipboard`] hands
/// it to the clipboard; freed on drop otherwise, so every early return and
/// every failed hand-off releases it.
struct GlobalBuffer(HGLOBAL);

impl GlobalBuffer {
    fn from_bytes(data: &[u8]) -> Result<Self, ClipboardError> {
        // SAFETY: GlobalAlloc with GMEM_MOVEABLE is the required allocation
        // strategy for clipboard data; failure is a NULL return.
        let handle: HGLOBAL = unsafe { GlobalAlloc(GMEM_MOVEABLE, data.len()) };
        if handle.is_null() {
            return Err(ClipboardError::io_other("GlobalAlloc failed"));
        }
        let buf = Self(handle);
        let locked = LockedHandle::new(buf.0)?;
        // SAFETY: `locked.ptr()` is a writable allocation of `data.len()`
        // bytes from `GlobalLock`, and `data` is `data.len()` readable bytes
        // of Rust memory, so the regions cannot overlap.
        unsafe {
            std::ptr::copy_nonoverlapping(data.as_ptr(), locked.ptr().cast::<u8>(), data.len());
        }
        // Unlocked before the block can be handed to the clipboard.
        drop(locked);
        Ok(buf)
    }
}

impl Drop for GlobalBuffer {
    fn drop(&mut self) {
        // SAFETY: `self.0` came from `GlobalAlloc` and is still ours: the one
        // place the clipboard takes ownership, `replace_clipboard`, forgets
        // the buffer instead of dropping it.
        unsafe {
            GlobalFree(self.0);
        }
    }
}

/// Replace the clipboard's contents with `items`, one block per format.
///
/// Takes the blocks already allocated and filled, so nothing that can fail on
/// our side runs after `EmptyClipboard` has destroyed what was there before:
/// a failed allocation leaves the old contents intact. Only a failing
/// `SetClipboardData` can still leave the clipboard holding the formats set
/// before it.
fn replace_clipboard(items: Vec<(UINT, GlobalBuffer)>) -> Result<(), ClipboardError> {
    let _guard = ClipboardOpen::new()?;

    // SAFETY: EmptyClipboard requires the clipboard to be open (guaranteed by
    // `_guard` above). It frees all existing data and clears the owner field.
    let ok = unsafe { EmptyClipboard() };
    if ok == 0 {
        return Err(ClipboardError::io_other("EmptyClipboard failed"));
    }

    for (format, buf) in items {
        // SAFETY: the clipboard is open (`_guard`) and was emptied by us, and
        // `buf.0` is an unlocked GMEM_MOVEABLE block holding `format` data.
        // On success the clipboard owns the block, so it must not be freed.
        let result = unsafe { SetClipboardData(format, buf.0) };
        if result.is_null() {
            // Ownership did not transfer: `buf` (and every block not yet
            // handed over) is freed as it drops.
            return Err(ClipboardError::io_other("SetClipboardData failed"));
        }
        std::mem::forget(buf);
    }

    // `_guard` drops here → `CloseClipboard()` called.
    Ok(())
}

// ---------------------------------------------------------------------------
// CF_UNICODETEXT helpers.
// ---------------------------------------------------------------------------

/// Write UTF-8 `bytes` to the clipboard as `CF_UNICODETEXT`.
fn set_text(bytes: &[u8]) -> Result<(), ClipboardError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| ClipboardError::io_other("clipboard text is not valid UTF-8"))?;

    // UTF-16 LE with an explicit null terminator.
    let utf16le: Vec<u8> = text
        .encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect();
    replace_clipboard(vec![(CF_UNICODETEXT, GlobalBuffer::from_bytes(&utf16le)?)])
}

/// Read `CF_UNICODETEXT` from the clipboard and return it as UTF-8 bytes.
fn get_text() -> Result<Vec<u8>, ClipboardError> {
    let _guard = ClipboardOpen::new()?;

    // SAFETY: `IsClipboardFormatAvailable` is safe to call at any time while
    // the clipboard is open. It does not alter clipboard state.
    let avail = unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT) };
    if avail == 0 {
        return Err(ClipboardError::UnsupportedMime);
    }

    // SAFETY: the clipboard is open and `CF_UNICODETEXT` is available. The
    // returned handle is owned by the clipboard — we must NOT free it.
    let handle = unsafe { GetClipboardData(CF_UNICODETEXT) };
    if handle.is_null() {
        return Err(ClipboardError::io_other("GetClipboardData failed"));
    }

    // Lock the handle (RAII releases on drop, even on early-return errors).
    let locked = LockedHandle::new(handle)?;

    // Determine the length in UTF-16 code units. `GlobalSize` returns bytes;
    // each UTF-16 unit is 2 bytes.
    //
    // SAFETY: `handle` is a valid, locked HGLOBAL. `GlobalSize` is safe to
    // call on any HGLOBAL returned by `GlobalAlloc` or the clipboard.
    let byte_size = unsafe { GlobalSize(handle) };
    let max_units = byte_size / 2;

    // SAFETY: `locked.ptr()` is a valid pointer to `byte_size` bytes
    // (≥ `max_units` UTF-16 code units). We only read up to `max_units`
    // units and stop at the first null terminator.
    let slice = unsafe { std::slice::from_raw_parts(locked.ptr().cast::<u16>(), max_units) };

    // Find the null terminator; exclude it from the string.
    let len_without_nul = slice.iter().position(|&c| c == 0).unwrap_or(max_units);
    let text_slice = &slice[..len_without_nul];

    let text = String::from_utf16(text_slice)
        .map_err(|_| ClipboardError::io_other("clipboard data is not valid UTF-16"))?;

    // `locked` drops here → GlobalUnlock. `_guard` drops next → CloseClipboard.
    Ok(text.into_bytes())
}

// ---------------------------------------------------------------------------
// Generic byte-exact clipboard helpers.
// ---------------------------------------------------------------------------
//
// `set_bytes` and `get_bytes` are the low-level primitives used by all
// registered-format paths (CF_HTML, CF_RTF). They do NOT do any encoding
// conversion — the caller supplies/receives exact bytes.
//
// The `CF_UNICODETEXT` path (text) keeps its own `set_text`/`get_text` with
// the UTF-8 ↔ UTF-16 LE conversion baked in.

/// Place `bytes` on the clipboard under `format`, replacing its contents.
fn set_bytes(format: UINT, bytes: &[u8]) -> Result<(), ClipboardError> {
    replace_clipboard(vec![(format, GlobalBuffer::from_bytes(bytes)?)])
}

/// Read `format` bytes from the clipboard, returning a heap-allocated copy.
///
/// Returns `ClipboardError::UnsupportedMime` if the format is not present.
fn get_bytes(format: UINT) -> Result<Vec<u8>, ClipboardError> {
    let _guard = ClipboardOpen::new()?;

    // SAFETY: IsClipboardFormatAvailable is safe to call while the clipboard
    // is open; it does not modify clipboard state.
    let avail = unsafe { IsClipboardFormatAvailable(format) };
    if avail == 0 {
        return Err(ClipboardError::UnsupportedMime);
    }

    // SAFETY: clipboard is open and the format is available. The returned
    // handle is owned by the clipboard — we must NOT free it.
    let handle = unsafe { GetClipboardData(format) };
    if handle.is_null() {
        return Err(ClipboardError::io_other("GetClipboardData failed"));
    }

    let locked = LockedHandle::new(handle)?;

    // SAFETY: `handle` is a valid locked HGLOBAL. `GlobalSize` returns the
    // allocation size in bytes.
    let byte_size = unsafe { GlobalSize(handle) };

    // SAFETY: `locked.ptr()` is a valid pointer to `byte_size` readable bytes.
    let slice = unsafe { std::slice::from_raw_parts(locked.ptr().cast::<u8>(), byte_size) };
    let out = slice.to_vec();

    // `locked` drops → GlobalUnlock. `_guard` drops → CloseClipboard.
    Ok(out)
}

// ---------------------------------------------------------------------------
// CF_HTML helpers.
// ---------------------------------------------------------------------------

/// Write an HTML string to the clipboard using the CF_HTML registered format.
fn set_html(html: &str) -> Result<(), ClipboardError> {
    let id = cf_html_format();
    if id == 0 {
        return Err(ClipboardError::io_other(
            "RegisterClipboardFormatW failed for CF_HTML",
        ));
    }
    let envelope = crate::cf_html::wrap(html);
    set_bytes(id, &envelope)
}

/// Read the CF_HTML registered format and return the inner fragment as UTF-8.
fn get_html() -> Result<Vec<u8>, ClipboardError> {
    let id = cf_html_format();
    if id == 0 {
        return Err(ClipboardError::io_other(
            "RegisterClipboardFormatW failed for CF_HTML",
        ));
    }
    let envelope = get_bytes(id)?;
    let fragment = crate::cf_html::unwrap(&envelope)?;
    Ok(fragment.into_bytes())
}

// ---------------------------------------------------------------------------
// CF_RTF helpers.
// ---------------------------------------------------------------------------

/// Write RTF bytes to the clipboard using the CF_RTF registered format.
fn set_rtf(bytes: &[u8]) -> Result<(), ClipboardError> {
    let id = cf_rtf_format();
    if id == 0 {
        return Err(ClipboardError::io_other(
            "RegisterClipboardFormatW failed for CF_RTF",
        ));
    }
    set_bytes(id, bytes)
}

/// Read the CF_RTF registered format and return the raw RTF bytes.
fn get_rtf() -> Result<Vec<u8>, ClipboardError> {
    let id = cf_rtf_format();
    if id == 0 {
        return Err(ClipboardError::io_other(
            "RegisterClipboardFormatW failed for CF_RTF",
        ));
    }
    get_bytes(id)
}

// ---------------------------------------------------------------------------
// CF_HDROP helpers.
// ---------------------------------------------------------------------------

/// Write a `text/uri-list` byte payload to the clipboard as CF_HDROP.
///
/// The incoming bytes are decoded as `text/uri-list` (RFC 2483). Only
/// `file://` URIs can be represented in CF_HDROP; a non-file URI returns
/// `ClipboardError::InvalidUri`.
fn set_uri_list(bytes: &[u8]) -> Result<(), ClipboardError> {
    let uris = crate::uri::decode_uri_list(bytes)?;
    let mut paths: Vec<std::path::PathBuf> = Vec::with_capacity(uris.len());
    for u in &uris {
        match u {
            Uri::File(p) => paths.push(p.clone()),
            Uri::Other(_) => {
                // CF_HDROP is files-only; non-file URIs cannot be represented.
                return Err(ClipboardError::InvalidUri);
            }
        }
    }
    let path_refs: Vec<&std::path::Path> = paths.iter().map(|p| p.as_path()).collect();
    let hdrop_bytes = crate::cf_hdrop::build(&path_refs)?;
    set_bytes(CF_HDROP, &hdrop_bytes)
}

/// Read CF_HDROP from the clipboard and return it as `text/uri-list` bytes.
fn get_uri_list() -> Result<Vec<u8>, ClipboardError> {
    let hdrop_bytes = get_bytes(CF_HDROP)?;
    let paths = crate::cf_hdrop::parse(&hdrop_bytes)?;
    let uris: Vec<Uri> = paths.into_iter().map(Uri::File).collect();
    crate::uri::encode_uri_list(&uris)
}

// ---------------------------------------------------------------------------
// PNG helpers.
// ---------------------------------------------------------------------------
//
// Modern apps on Windows register a `"PNG"` clipboard format and store raw PNG
// bytes. Legacy apps (e.g. older Office, some image editors) only consume
// `CF_DIBV5`. We always set both so any app can paste the image.
//
// On get, we prefer the `"PNG"` format (round-trip lossless) and fall back to
// `CF_DIBV5` when only legacy data is present.

/// Write PNG bytes to the clipboard as both `"PNG"` and `CF_DIBV5`.
///
/// The DIB conversion and both allocations happen before the clipboard is
/// touched, so any of them failing leaves it as it was.
fn set_png(bytes: &[u8]) -> Result<(), ClipboardError> {
    let png_id = cf_png_format();
    if png_id == 0 {
        return Err(ClipboardError::io_other(
            "RegisterClipboardFormatW failed for PNG",
        ));
    }
    let dib = crate::dib_png::png_to_dib(bytes)?;
    replace_clipboard(vec![
        (png_id, GlobalBuffer::from_bytes(bytes)?),
        (CF_DIBV5, GlobalBuffer::from_bytes(&dib)?),
    ])
}

/// Read PNG bytes from the clipboard.
///
/// Prefers the `"PNG"` registered format (raw passthrough). Falls back to
/// `CF_DIBV5` with DIB→PNG conversion. Returns `UnsupportedMime` if neither
/// format is present.
fn get_png() -> Result<Vec<u8>, ClipboardError> {
    let png_id = cf_png_format();

    let _guard = ClipboardOpen::new()?;

    // Try "PNG" registered format first.
    if png_id != 0 {
        // SAFETY: clipboard is open; IsClipboardFormatAvailable does not alter state.
        let avail = unsafe { IsClipboardFormatAvailable(png_id) };
        if avail != 0 {
            // SAFETY: clipboard is open and format is available; handle is clipboard-owned.
            let handle = unsafe { GetClipboardData(png_id) };
            if !handle.is_null() {
                let locked = LockedHandle::new(handle)?;
                // SAFETY: handle is a valid locked HGLOBAL.
                let byte_size = unsafe { GlobalSize(handle) };
                // SAFETY: locked.ptr() is valid for `byte_size` readable bytes.
                let slice =
                    unsafe { std::slice::from_raw_parts(locked.ptr().cast::<u8>(), byte_size) };
                return Ok(slice.to_vec());
            }
        }
    }

    // Fallback: CF_DIBV5 -> DIB-to-PNG conversion.
    // SAFETY: clipboard is open; IsClipboardFormatAvailable does not alter state.
    let avail = unsafe { IsClipboardFormatAvailable(CF_DIBV5) };
    if avail == 0 {
        return Err(ClipboardError::UnsupportedMime);
    }
    // SAFETY: clipboard is open and CF_DIBV5 is available; handle is clipboard-owned.
    let handle = unsafe { GetClipboardData(CF_DIBV5) };
    if handle.is_null() {
        return Err(ClipboardError::io_other(
            "GetClipboardData(CF_DIBV5) failed",
        ));
    }
    let locked = LockedHandle::new(handle)?;
    // SAFETY: handle is a valid locked HGLOBAL.
    let byte_size = unsafe { GlobalSize(handle) };
    // SAFETY: locked.ptr() is valid for `byte_size` readable bytes.
    let slice = unsafe { std::slice::from_raw_parts(locked.ptr().cast::<u8>(), byte_size) };
    let dib = slice.to_vec();
    drop(locked);
    crate::dib_png::dib_to_png(&dib)
}

// ---------------------------------------------------------------------------
// Backend impl.
// ---------------------------------------------------------------------------

/// Windows clipboard backend. Unit struct — all state lives in the Win32
/// clipboard.
pub struct WindowsBackend;

impl WindowsBackend {
    #[allow(dead_code)]
    pub(crate) fn new() -> Self {
        Self
    }
}

impl Backend for WindowsBackend {
    fn kind(&self) -> crate::BackendKind {
        crate::BackendKind::Windows
    }

    fn capabilities(&self) -> crate::Capabilities {
        // Win32 clipboard supports full sync matrix. No PRIMARY (Win concept
        // absent). Async stays default (UnsupportedAsync) — no native async API.
        crate::Capabilities::WRITE
            | crate::Capabilities::READ
            | crate::Capabilities::CLEAR
            | crate::Capabilities::AVAILABLE
            | crate::Capabilities::IMAGE
            | crate::Capabilities::RICH_TEXT
            | crate::Capabilities::URI_LIST
    }

    fn set(&self, sel: Selection, mime: MimeType, bytes: &[u8]) -> Result<(), ClipboardError> {
        // Windows has no concept of a primary selection.
        if sel != Selection::Clipboard {
            return Err(ClipboardError::UnsupportedMime);
        }
        match mime {
            MimeType::Text => set_text(bytes),
            MimeType::Html => {
                let html = std::str::from_utf8(bytes)
                    .map_err(|_| ClipboardError::io_other("HTML payload is not valid UTF-8"))?;
                set_html(html)
            }
            MimeType::Rtf => set_rtf(bytes),
            MimeType::UriList => set_uri_list(bytes),
            MimeType::Png => set_png(bytes),
            MimeType::Custom(_) => Err(ClipboardError::UnsupportedMime),
        }
    }

    fn get(&self, sel: Selection, mime: MimeType) -> Result<Vec<u8>, ClipboardError> {
        // Windows has no concept of a primary selection.
        if sel != Selection::Clipboard {
            return Err(ClipboardError::UnsupportedMime);
        }
        match mime {
            MimeType::Text => get_text(),
            MimeType::Html => get_html(),
            MimeType::Rtf => get_rtf(),
            MimeType::UriList => get_uri_list(),
            MimeType::Png => get_png(),
            _ => Err(ClipboardError::UnsupportedMime),
        }
    }

    fn clear(&self, sel: Selection) -> Result<(), ClipboardError> {
        // Windows has no concept of a primary selection.
        if sel != Selection::Clipboard {
            return Err(ClipboardError::UnsupportedMime);
        }
        let _guard = ClipboardOpen::new()?;
        // SAFETY: the clipboard is open (`_guard`). `EmptyClipboard` frees all
        // existing clipboard data and clears the owner field. This is the
        // documented way to clear the clipboard.
        let ok = unsafe { EmptyClipboard() };
        if ok == 0 {
            return Err(ClipboardError::io_other("EmptyClipboard failed"));
        }
        Ok(())
    }

    fn available(&self, sel: Selection) -> Result<Vec<MimeType>, ClipboardError> {
        // Windows has no concept of a primary selection — match set/get/clear
        // and surface `UnsupportedMime` so callers get a clear signal rather
        // than an empty list that misleadingly implies "primary works but is
        // empty".
        if sel != Selection::Clipboard {
            return Err(ClipboardError::UnsupportedMime);
        }
        let _guard = ClipboardOpen::new()?;
        let mut out = Vec::new();
        let html_id = cf_html_format();
        let rtf_id = cf_rtf_format();
        let png_id = cf_png_format();
        let mut png_seen = false;
        let mut fmt: UINT = 0;
        loop {
            // SAFETY: passing 0 (or the previous format) to `EnumClipboardFormats`
            // while the clipboard is open is the documented enumeration pattern.
            // It returns 0 when enumeration is complete (no more formats) or on
            // error; both cases terminate the loop identically.
            fmt = unsafe { EnumClipboardFormats(fmt) };
            if fmt == 0 {
                break;
            }
            if fmt == CF_UNICODETEXT {
                out.push(MimeType::Text);
            } else if fmt == CF_HDROP {
                out.push(MimeType::UriList);
            } else if html_id != 0 && fmt == html_id {
                out.push(MimeType::Html);
            } else if rtf_id != 0 && fmt == rtf_id {
                out.push(MimeType::Rtf);
            } else if (png_id != 0 && fmt == png_id) || fmt == CF_DIBV5 {
                // Report MimeType::Png at most once regardless of which format
                // (or both) the clipboard contains.
                if !png_seen {
                    out.push(MimeType::Png);
                    png_seen = true;
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_buffer_holds_a_copy_of_the_bytes() {
        let data = b"clipboard \x00 bytes";
        let buf = GlobalBuffer::from_bytes(data).unwrap();
        let locked = LockedHandle::new(buf.0).unwrap();
        // SAFETY: `buf.0` is a live, locked GlobalAlloc block.
        let size = unsafe { GlobalSize(buf.0) };
        assert!(size >= data.len());
        // SAFETY: the locked block is readable for `size` >= `data.len()` bytes.
        let copy = unsafe { std::slice::from_raw_parts(locked.ptr().cast::<u8>(), data.len()) };
        assert_eq!(copy, data);
    }

    /// Every format through the real Win32 clipboard, in one test so nothing
    /// races on it. Puts the text that was there back afterwards.
    #[test]
    #[ignore = "replaces the contents of the real system clipboard"]
    fn every_format_round_trips_through_the_system_clipboard() {
        let saved = get_text().ok();

        set_text("hjkl \u{2713} \u{6587}\u{5b57}\r\nline two".as_bytes()).unwrap();
        assert_eq!(
            get_text().unwrap(),
            "hjkl \u{2713} \u{6587}\u{5b57}\r\nline two".as_bytes()
        );

        set_html("<b>bold</b> text").unwrap();
        assert_eq!(get_html().unwrap(), b"<b>bold</b> text");

        set_rtf(br"{\rtf1 plain}").unwrap();
        assert_eq!(get_rtf().unwrap(), br"{\rtf1 plain}");

        let png = crate::dib_png::tests::build_png(2, 1, 4, &[255, 0, 0, 255, 0, 0, 255, 128]);
        set_png(&png).unwrap();
        assert_eq!(get_png().unwrap(), png);
        let formats = WindowsBackend::new()
            .available(Selection::Clipboard)
            .unwrap();
        assert_eq!(formats, [MimeType::Png]);

        // A payload that fails before the clipboard is touched leaves it as
        // it was.
        assert!(set_png(b"not a png").is_err());
        assert_eq!(get_png().unwrap(), png);

        match saved {
            Some(text) => set_text(&text).unwrap(),
            None => WindowsBackend::new().clear(Selection::Clipboard).unwrap(),
        }
    }
}
