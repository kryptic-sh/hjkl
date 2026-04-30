//! Dynamic library loaders for libxcb and libwayland-client.
//!
//! Symbols are stored in a `OnceLock<Option<XcbFns>>` so the load happens at
//! most once per process. Missing libraries return
//! [`ClipboardError::LibNotFound`].

use std::ffi::{CStr, c_char, c_int, c_uint, c_void};
use std::sync::OnceLock;

use crate::ClipboardError;

// ---------------------------------------------------------------------------
// XCB opaque types (ZSTs via uninhabited enums)
// ---------------------------------------------------------------------------

/// Opaque XCB connection. Never constructed directly; always behind `*mut`.
pub enum XcbConnection {}

/// Opaque XCB setup struct returned by `xcb_get_setup`.
pub enum XcbSetup {}

/// Opaque XCB screen struct. Accessed via the iterator.
pub enum XcbScreen {}

/// Opaque XCB generic error returned by reply functions.
pub enum XcbGenericError {}

/// Opaque XCB generic event.
pub enum XcbGenericEvent {}

// ---------------------------------------------------------------------------
// XCB C-ABI structs
// ---------------------------------------------------------------------------

/// Cookie returned by `xcb_intern_atom`. Carries only a sequence number.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct XcbInternAtomCookie {
    pub sequence: c_uint,
}

/// Reply from `xcb_intern_atom_reply`. Must be freed with `libc::free`.
#[repr(C)]
pub struct XcbInternAtomReply {
    pub response_type: u8,
    pub pad0: u8,
    pub sequence: u16,
    pub length: u32,
    pub atom: u32,
}

/// Iterator over XCB screen structs. Returned by value from
/// `xcb_setup_roots_iterator`. On x86_64/aarch64 the 16-byte struct fits in
/// two registers so the C ABI handles this as a normal return value.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct XcbScreenIterator {
    pub data: *mut XcbScreen,
    pub rem: c_int,
    pub index: c_int,
}

/// Cookie for `xcb_get_selection_owner`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct XcbGetSelectionOwnerCookie {
    pub sequence: c_uint,
}

/// Reply from `xcb_get_selection_owner_reply`. Must be freed with `libc::free`.
#[repr(C)]
pub struct XcbGetSelectionOwnerReply {
    pub response_type: u8,
    pub pad0: u8,
    pub sequence: u16,
    pub length: u32,
    pub owner: u32,
}

/// Cookie for `xcb_get_property`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct XcbGetPropertyCookie {
    pub sequence: c_uint,
}

/// Reply from `xcb_get_property_reply`. Must be freed with `libc::free`.
#[repr(C)]
pub struct XcbGetPropertyReply {
    pub response_type: u8,
    pub format: u8,
    pub sequence: u16,
    pub length: u32,
    pub r#type: u32,
    pub bytes_after: u32,
    pub value_len: u32,
    pub pad0: [u8; 12],
}

/// Void cookie (for unchecked requests). Also used by `xcb_request_check`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct XcbVoidCookie {
    pub sequence: c_uint,
}

// ---------------------------------------------------------------------------
// Function-pointer table
// ---------------------------------------------------------------------------

/// All libxcb function pointers needed across phases 5a-5d.
///
/// Loaded once via `xcb_fns()` and then held for the process lifetime.
pub struct XcbFns {
    // Phase 5a — connection + atoms
    pub xcb_connect:
        unsafe extern "C" fn(display: *const c_char, screen_p: *mut c_int) -> *mut XcbConnection,
    pub xcb_disconnect: unsafe extern "C" fn(c: *mut XcbConnection),
    pub xcb_connection_has_error: unsafe extern "C" fn(c: *mut XcbConnection) -> c_int,
    pub xcb_get_setup: unsafe extern "C" fn(c: *mut XcbConnection) -> *const XcbSetup,
    pub xcb_setup_roots_iterator: unsafe extern "C" fn(r: *const XcbSetup) -> XcbScreenIterator,
    pub xcb_intern_atom: unsafe extern "C" fn(
        c: *mut XcbConnection,
        only_if_exists: u8,
        name_len: u16,
        name: *const c_char,
    ) -> XcbInternAtomCookie,
    pub xcb_intern_atom_reply: unsafe extern "C" fn(
        c: *mut XcbConnection,
        cookie: XcbInternAtomCookie,
        e: *mut *mut XcbGenericError,
    ) -> *mut XcbInternAtomReply,
    pub xcb_flush: unsafe extern "C" fn(c: *mut XcbConnection) -> c_int,
    pub xcb_generate_id: unsafe extern "C" fn(c: *mut XcbConnection) -> u32,

    // Phase 5b — window + selection ownership
    pub xcb_create_window: unsafe extern "C" fn(
        c: *mut XcbConnection,
        depth: u8,
        wid: u32,
        parent: u32,
        x: i16,
        y: i16,
        width: u16,
        height: u16,
        border_width: u16,
        class: u16,
        visual: u32,
        value_mask: u32,
        value_list: *const c_void,
    ) -> XcbVoidCookie,
    pub xcb_set_selection_owner: unsafe extern "C" fn(
        c: *mut XcbConnection,
        owner: u32,
        selection: u32,
        time: u32,
    ) -> XcbVoidCookie,
    pub xcb_get_selection_owner:
        unsafe extern "C" fn(c: *mut XcbConnection, selection: u32) -> XcbGetSelectionOwnerCookie,
    pub xcb_get_selection_owner_reply: unsafe extern "C" fn(
        c: *mut XcbConnection,
        cookie: XcbGetSelectionOwnerCookie,
        e: *mut *mut XcbGenericError,
    ) -> *mut XcbGetSelectionOwnerReply,
    pub xcb_change_property: unsafe extern "C" fn(
        c: *mut XcbConnection,
        mode: u8,
        window: u32,
        property: u32,
        r#type: u32,
        format: u8,
        data_len: u32,
        data: *const c_void,
    ) -> XcbVoidCookie,
    pub xcb_send_event: unsafe extern "C" fn(
        c: *mut XcbConnection,
        propagate: u8,
        destination: u32,
        event_mask: u32,
        event: *const c_char,
    ) -> XcbVoidCookie,
    pub xcb_wait_for_event: unsafe extern "C" fn(c: *mut XcbConnection) -> *mut XcbGenericEvent,
    pub xcb_poll_for_event: unsafe extern "C" fn(c: *mut XcbConnection) -> *mut XcbGenericEvent,
    pub xcb_request_check:
        unsafe extern "C" fn(c: *mut XcbConnection, cookie: XcbVoidCookie) -> *mut XcbGenericError,

    // Phase 5c — property read
    pub xcb_convert_selection: unsafe extern "C" fn(
        c: *mut XcbConnection,
        requestor: u32,
        selection: u32,
        target: u32,
        property: u32,
        time: u32,
    ) -> XcbVoidCookie,
    pub xcb_get_property: unsafe extern "C" fn(
        c: *mut XcbConnection,
        delete: u8,
        window: u32,
        property: u32,
        r#type: u32,
        long_offset: u32,
        long_length: u32,
    ) -> XcbGetPropertyCookie,
    pub xcb_get_property_reply: unsafe extern "C" fn(
        c: *mut XcbConnection,
        cookie: XcbGetPropertyCookie,
        e: *mut *mut XcbGenericError,
    ) -> *mut XcbGetPropertyReply,
    pub xcb_get_property_value: unsafe extern "C" fn(r: *const XcbGetPropertyReply) -> *mut c_void,
    pub xcb_get_property_value_length: unsafe extern "C" fn(r: *const XcbGetPropertyReply) -> c_int,
    pub xcb_delete_property:
        unsafe extern "C" fn(c: *mut XcbConnection, window: u32, property: u32) -> XcbVoidCookie,
}

// SAFETY: All members are function pointers loaded from a shared library.
// libxcb's documentation states its functions are thread-safe with respect to
// the connection object. We never race on `XcbFns` itself (it's in a
// `OnceLock` and never mutated after init).
unsafe impl Send for XcbFns {}
// SAFETY: Same reasoning as Send — immutable after construction, fn ptrs are
// stateless.
unsafe impl Sync for XcbFns {}

// ---------------------------------------------------------------------------
// dlopen / dlsym helpers
// ---------------------------------------------------------------------------

const LIBXCB: &CStr = c"libxcb.so.1";

/// Load a single symbol from a `dlopen` handle.
///
/// # Safety
///
/// `handle` must be a valid handle returned by `libc::dlopen`. `name` must be
/// a valid NUL-terminated C string. The returned pointer is cast to `T`; the
/// caller must ensure `T` matches the actual symbol type in the library.
unsafe fn load_sym<T: Copy>(handle: *mut c_void, name: &CStr) -> Option<T> {
    // SAFETY: handle is valid, name is NUL-terminated.
    let ptr = unsafe { libc::dlsym(handle, name.as_ptr()) };
    if ptr.is_null() {
        return None;
    }
    // SAFETY: `ptr` is non-null and points to the symbol. `T` is a function
    // pointer type whose size equals `*mut c_void` on all supported targets.
    Some(unsafe { std::mem::transmute_copy(&ptr) })
}

/// Macro for loading a symbol; returns `None` from the enclosing function if
/// the symbol is absent.
macro_rules! sym {
    ($handle:expr, $name:literal) => {{
        // SAFETY: the concat literal ends with exactly one '\0' byte —
        // `CStr::from_bytes_with_nul_unchecked` requires this. `load_sym`
        // requires a valid dlopen handle and a NUL-terminated name; both
        // conditions are met here.
        let cstr = unsafe { CStr::from_bytes_with_nul_unchecked(concat!($name, "\0").as_bytes()) };
        match unsafe { load_sym($handle, cstr) } {
            Some(f) => f,
            None => return None,
        }
    }};
}

/// Load libxcb and all required symbols. Returns `None` if the library or any
/// mandatory symbol is absent.
fn try_load_xcb() -> Option<XcbFns> {
    // SAFETY: `LIBXCB` is a valid NUL-terminated C string. `RTLD_LAZY |
    // RTLD_GLOBAL` defers symbol resolution; GLOBAL allows XCB extension libs
    // to share the same handle pool.
    let handle = unsafe { libc::dlopen(LIBXCB.as_ptr(), libc::RTLD_LAZY | libc::RTLD_GLOBAL) };
    if handle.is_null() {
        return None;
    }

    Some(XcbFns {
        xcb_connect: sym!(handle, "xcb_connect"),
        xcb_disconnect: sym!(handle, "xcb_disconnect"),
        xcb_connection_has_error: sym!(handle, "xcb_connection_has_error"),
        xcb_get_setup: sym!(handle, "xcb_get_setup"),
        xcb_setup_roots_iterator: sym!(handle, "xcb_setup_roots_iterator"),
        xcb_intern_atom: sym!(handle, "xcb_intern_atom"),
        xcb_intern_atom_reply: sym!(handle, "xcb_intern_atom_reply"),
        xcb_flush: sym!(handle, "xcb_flush"),
        xcb_generate_id: sym!(handle, "xcb_generate_id"),
        xcb_create_window: sym!(handle, "xcb_create_window"),
        xcb_set_selection_owner: sym!(handle, "xcb_set_selection_owner"),
        xcb_get_selection_owner: sym!(handle, "xcb_get_selection_owner"),
        xcb_get_selection_owner_reply: sym!(handle, "xcb_get_selection_owner_reply"),
        xcb_change_property: sym!(handle, "xcb_change_property"),
        xcb_send_event: sym!(handle, "xcb_send_event"),
        xcb_wait_for_event: sym!(handle, "xcb_wait_for_event"),
        xcb_poll_for_event: sym!(handle, "xcb_poll_for_event"),
        xcb_request_check: sym!(handle, "xcb_request_check"),
        xcb_convert_selection: sym!(handle, "xcb_convert_selection"),
        xcb_get_property: sym!(handle, "xcb_get_property"),
        xcb_get_property_reply: sym!(handle, "xcb_get_property_reply"),
        xcb_get_property_value: sym!(handle, "xcb_get_property_value"),
        xcb_get_property_value_length: sym!(handle, "xcb_get_property_value_length"),
        xcb_delete_property: sym!(handle, "xcb_delete_property"),
    })
}

// ---------------------------------------------------------------------------
// Public accessor
// ---------------------------------------------------------------------------

static XCB_FNS: OnceLock<Option<XcbFns>> = OnceLock::new();

/// Return a reference to the loaded XCB function-pointer table, or
/// `ClipboardError::LibNotFound` if libxcb is not available.
pub(crate) fn xcb_fns() -> Result<&'static XcbFns, ClipboardError> {
    XCB_FNS
        .get_or_init(try_load_xcb)
        .as_ref()
        .ok_or(ClipboardError::LibNotFound)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke-test: libxcb is present on any Linux dev box; all symbols must
    /// resolve. If libxcb is somehow absent the test is skipped via `skip!`.
    #[test]
    fn xcb_dlopen_smoke() {
        match xcb_fns() {
            Ok(_) => {
                // All symbols resolved; nothing more to assert here —
                // individual field types are verified by the compiler.
            }
            Err(ClipboardError::LibNotFound) => {
                eprintln!("SKIP xcb_dlopen_smoke: libxcb.so.1 not found");
            }
            Err(e) => panic!("unexpected error: {e}"),
        }
    }
}
