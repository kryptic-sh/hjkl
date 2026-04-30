//! Windows clipboard backend via Win32 user32/kernel32.
//!
//! Uses `OpenClipboard(NULL)`, `EmptyClipboard`, `GlobalAlloc(GMEM_MOVEABLE)`,
//! and `SetClipboardData` / `GetClipboardData` for all supported formats.
//!
//! No `winapi` or `windows-sys` dependency — raw `extern "system"` FFI only.

use std::ffi::c_void;

use crate::{ClipboardError, MimeType, Selection};

use super::Backend;

// ---------------------------------------------------------------------------
// Win32 type aliases — integer-backed, no external crates.
// ---------------------------------------------------------------------------

// Win32 conventional names are all-caps; suppress the clippy lint that wants
// camel-case acronyms. The names are intentionally mirroring the C API so
// that diffs against MSDN documentation are easy to read.
#[allow(clippy::upper_case_acronyms, non_camel_case_types)]
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
    fn GetLastError() -> DWORD;
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
            return Err(ClipboardError::Io(std::io::Error::other(
                "OpenClipboard failed",
            )));
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

// ---------------------------------------------------------------------------
// CF_UNICODETEXT helpers.
// ---------------------------------------------------------------------------

/// Write UTF-8 `bytes` to the clipboard as `CF_UNICODETEXT`.
fn set_text(bytes: &[u8]) -> Result<(), ClipboardError> {
    let text = std::str::from_utf8(bytes).map_err(|_| {
        ClipboardError::Io(std::io::Error::other("clipboard text is not valid UTF-8"))
    })?;

    // Encode as UTF-16 LE with an explicit null terminator.
    let utf16: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let byte_len = utf16.len() * 2;

    let _guard = ClipboardOpen::new()?;

    // SAFETY: EmptyClipboard requires the clipboard to be open (guaranteed by
    // `_guard` above). It frees all existing data and clears the owner field.
    let ok = unsafe { EmptyClipboard() };
    if ok == 0 {
        return Err(ClipboardError::Io(std::io::Error::other(
            "EmptyClipboard failed",
        )));
    }

    // SAFETY: GlobalAlloc with GMEM_MOVEABLE is the required allocation
    // strategy for clipboard data. The size is derived from a `Vec<u16>` whose
    // length is bounded by the input string length; no overflow is possible for
    // any string that fits in memory.
    let handle: HGLOBAL = unsafe { GlobalAlloc(GMEM_MOVEABLE, byte_len) };
    if handle.is_null() {
        return Err(ClipboardError::Io(std::io::Error::other(
            "GlobalAlloc failed",
        )));
    }

    // SAFETY: `handle` is a valid HGLOBAL returned by `GlobalAlloc` above.
    // `GlobalLock` returns a pointer to the first byte of the allocation.
    let ptr = unsafe { GlobalLock(handle) };
    if ptr.is_null() {
        // SAFETY: `handle` is still our allocation; GlobalFree releases it.
        unsafe { GlobalFree(handle) };
        return Err(ClipboardError::Io(std::io::Error::other(
            "GlobalLock failed",
        )));
    }

    // SAFETY: `ptr` is a valid, writable allocation of `byte_len` bytes
    // obtained from `GlobalLock`. `utf16.as_ptr()` points to `utf16.len()`
    // valid `u16` values. The regions do not overlap (one is on the system
    // heap, the other is our stack-allocated Vec).
    unsafe {
        std::ptr::copy_nonoverlapping(utf16.as_ptr(), ptr.cast::<u16>(), utf16.len());
    }

    // SAFETY: `handle` is locked. `GlobalUnlock` decrements the lock count.
    // The return value indicates remaining locks; we ignore it here because the
    // handle is about to be transferred to the clipboard.
    unsafe {
        GlobalUnlock(handle);
    }

    // SAFETY: the clipboard is open (`_guard`), `EmptyClipboard` was called,
    // and `handle` holds a valid GMEM_MOVEABLE allocation with CF_UNICODETEXT
    // data. On success the clipboard takes ownership of the handle — we must
    // NOT free it. On failure (NULL return) we must free it ourselves.
    let result = unsafe { SetClipboardData(CF_UNICODETEXT, handle) };
    if result.is_null() {
        // SAFETY: `SetClipboardData` failed, so ownership did not transfer.
        // We free the allocation to avoid a leak.
        unsafe { GlobalFree(handle) };
        return Err(ClipboardError::Io(std::io::Error::other(
            "SetClipboardData failed",
        )));
    }

    // `_guard` drops here → `CloseClipboard()` called.
    Ok(())
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
        return Err(ClipboardError::Io(std::io::Error::other(
            "GetClipboardData failed",
        )));
    }

    // SAFETY: `handle` is a valid clipboard-owned HGLOBAL. `GlobalLock` gives
    // us a temporary pointer; we must call `GlobalUnlock` before the clipboard
    // is closed.
    let ptr = unsafe { GlobalLock(handle) };
    if ptr.is_null() {
        return Err(ClipboardError::Io(std::io::Error::other(
            "GlobalLock failed",
        )));
    }

    // Determine the length in UTF-16 code units. `GlobalSize` returns bytes;
    // each UTF-16 unit is 2 bytes. We cap at `size / 2` and then scan for the
    // null terminator to exclude it from the string.
    //
    // SAFETY: `handle` is a valid, locked HGLOBAL. `GlobalSize` is safe to
    // call on any HGLOBAL returned by `GlobalAlloc` or the clipboard.
    let byte_size = unsafe { GlobalSize(handle) };
    let max_units = byte_size / 2;

    // SAFETY: `ptr` is a valid pointer to `byte_size` bytes (≥ `max_units`
    // UTF-16 code units). We only read up to `max_units` units and stop at the
    // first null terminator, so we never read past the allocation.
    let slice = unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), max_units) };

    // Find the null terminator; exclude it from the string.
    let len_without_nul = slice.iter().position(|&c| c == 0).unwrap_or(max_units);
    let text_slice = &slice[..len_without_nul];

    let text = String::from_utf16(text_slice).map_err(|_| {
        ClipboardError::Io(std::io::Error::other("clipboard data is not valid UTF-16"))
    })?;

    // SAFETY: `handle` is still locked. `GlobalUnlock` decrements the lock
    // count. We are done reading so unlocking is correct here.
    unsafe {
        GlobalUnlock(handle);
    }

    // `_guard` drops here → `CloseClipboard()` called.
    Ok(text.into_bytes())
}

// ---------------------------------------------------------------------------
// Backend impl.
// ---------------------------------------------------------------------------

pub(crate) struct WindowsBackend;

impl WindowsBackend {
    pub(crate) fn new() -> Self {
        Self
    }
}

impl Backend for WindowsBackend {
    fn set(&self, sel: Selection, mime: MimeType, bytes: &[u8]) -> Result<(), ClipboardError> {
        // Windows has no concept of a primary selection.
        if sel != Selection::Clipboard {
            return Err(ClipboardError::UnsupportedMime);
        }
        match mime {
            MimeType::Text => set_text(bytes),
            // Phases 3b/3c/3d will add CF_HTML, CF_RTF, CF_HDROP, DIB↔PNG.
            MimeType::Html | MimeType::Rtf | MimeType::UriList | MimeType::Png => {
                Err(ClipboardError::UnsupportedMime)
            }
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
            // Phases 3b/3c/3d will fill these in.
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
            return Err(ClipboardError::Io(std::io::Error::other(
                "EmptyClipboard failed",
            )));
        }
        Ok(())
    }

    fn available(&self, sel: Selection) -> Result<Vec<MimeType>, ClipboardError> {
        // Windows has no concept of a primary selection — return empty rather
        // than an error, consistent with the OSC 52 backend convention.
        if sel != Selection::Clipboard {
            return Ok(vec![]);
        }
        let _guard = ClipboardOpen::new()?;
        let mut out = Vec::new();
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
            // Phase 3a maps only CF_UNICODETEXT → MimeType::Text.
            // Phases 3b/3c/3d will add CF_HTML, CF_RTF, CF_HDROP, DIB↔PNG.
            if fmt == CF_UNICODETEXT {
                out.push(MimeType::Text);
            }
        }
        Ok(out)
    }
}
