//! macOS clipboard backend via NSPasteboard (raw `objc_msgSend`).
//!
//! Links AppKit + Foundation frameworks and libobjc. The `objc_msgSend`
//! calling convention differs between x86_64 and ARM64 — each call site
//! must cast the function pointer to the exact (self, sel, args...) -> Ret
//! signature, matching the Objective-C method prototype precisely.
//!
//! No `objc`, `objc2`, or `cocoa-foundation` crate — raw FFI only.

use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::OnceLock;

use crate::{ClipboardError, MimeType, Selection};

use super::Backend;

// ---------------------------------------------------------------------------
// Type aliases.
// ---------------------------------------------------------------------------

/// Pointer-sized Objective-C object reference.
type Id = *mut c_void;

/// Objective-C class (same representation as `Id`).
type Class = *mut c_void;

/// Opaque selector pointer.
type Sel = *const c_void;

/// NSUInteger — matches pointer width on both x86_64 and ARM64.
type NSUInteger = usize;

// ---------------------------------------------------------------------------
// Framework + libobjc linking.
// ---------------------------------------------------------------------------

#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {}

#[link(name = "Foundation", kind = "framework")]
unsafe extern "C" {}

#[link(name = "objc")]
unsafe extern "C" {
    /// Register (or look up) an Objective-C selector by name.
    fn sel_registerName(name: *const c_char) -> Sel;

    /// Look up an Objective-C class by name. Returns NULL if not found.
    fn objc_getClass(name: *const c_char) -> Class;

    /// Universal Objective-C message-send stub. We never call this signature
    /// directly — each call site transmutes the pointer to the exact prototype
    /// matching the called method's (self, sel, args...) -> Ret signature.
    /// ARM64 ABI: all arguments including self and sel go in registers; the
    /// prototype must be exact. x86_64 ABI: same principle, different
    /// registers. Wrong prototype = undefined behaviour / segfault.
    fn objc_msgSend();
}

// ---------------------------------------------------------------------------
// msg helpers — transmute per call site.
// ---------------------------------------------------------------------------
//
// Each helper transmutes `objc_msgSend` to the concrete signature matching
// the number of extra arguments. All types involved are pointer-sized or
// `usize`, so the C calling convention matches Apple's ABI on both targets
// without surprises. Do NOT use these for methods that return value types
// larger than 16 bytes or take float/SIMD arguments — none of our calls do.

/// Message with no extra arguments.
unsafe fn msg0<R>(obj: Id, sel: Sel) -> R {
    // SAFETY: `obj` is a valid Objective-C object, `sel` is a registered
    // selector. The return type `R` must exactly match the ObjC method's
    // return type. ARM64/x86_64 ABI requires this exact prototype cast.
    let f: unsafe extern "C" fn(Id, Sel) -> R =
        // SAFETY: transmuting the stub to the concrete signature — see module doc.
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    // SAFETY: the transmuted pointer has the correct ABI for this call.
    unsafe { f(obj, sel) }
}

/// Message with one extra argument.
unsafe fn msg1<A, R>(obj: Id, sel: Sel, a: A) -> R {
    // SAFETY: same as `msg0`; A must exactly match the first argument type.
    let f: unsafe extern "C" fn(Id, Sel, A) -> R =
        // SAFETY: transmuting the stub — see module doc.
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    // SAFETY: the transmuted pointer has the correct ABI for this call.
    unsafe { f(obj, sel, a) }
}

/// Message with two extra arguments.
unsafe fn msg2<A, B, R>(obj: Id, sel: Sel, a: A, b: B) -> R {
    // SAFETY: same as `msg0`; A and B must match the method's argument types.
    let f: unsafe extern "C" fn(Id, Sel, A, B) -> R =
        // SAFETY: transmuting the stub — see module doc.
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    // SAFETY: the transmuted pointer has the correct ABI for this call.
    unsafe { f(obj, sel, a, b) }
}

// ---------------------------------------------------------------------------
// Selector cache.
// ---------------------------------------------------------------------------
//
// Selectors are stable for the process lifetime (Apple ABI guarantee).
// Store as `usize` because raw pointers are not `Send`; cast back at use.

macro_rules! sel_cached {
    ($fn_name:ident, $name:literal) => {
        fn $fn_name() -> Sel {
            static S: OnceLock<usize> = OnceLock::new();
            // SAFETY: the literal ends with `\0`, satisfying the C-string
            // contract. `sel_registerName` is safe to call from any thread;
            // it returns a pointer stable for the process lifetime.
            *S.get_or_init(|| unsafe {
                sel_registerName(concat!($name, "\0").as_ptr().cast()) as usize
            }) as Sel
        }
    };
}

sel_cached!(sel_general_pasteboard, "generalPasteboard");
sel_cached!(sel_clear_contents, "clearContents");
sel_cached!(sel_set_data_for_type, "setData:forType:");
sel_cached!(sel_data_for_type, "dataForType:");
sel_cached!(sel_types, "types");
sel_cached!(sel_count, "count");
sel_cached!(sel_object_at_index, "objectAtIndex:");
sel_cached!(sel_utf8_string, "UTF8String");
sel_cached!(sel_length, "length");
sel_cached!(sel_bytes, "bytes");
sel_cached!(sel_data_with_bytes_length, "dataWithBytes:length:");
sel_cached!(sel_string_with_utf8_string, "stringWithUTF8String:");

// ---------------------------------------------------------------------------
// Class cache.
// ---------------------------------------------------------------------------

macro_rules! class_cached {
    ($fn_name:ident, $name:literal) => {
        fn $fn_name() -> Class {
            static C: OnceLock<usize> = OnceLock::new();
            // SAFETY: the literal ends with `\0`. `objc_getClass` is thread-safe
            // and returns a stable pointer (NULL if the class is absent, which
            // would indicate a misconfigured SDK linkage).
            *C.get_or_init(|| unsafe {
                objc_getClass(concat!($name, "\0").as_ptr().cast()) as usize
            }) as Class
        }
    };
}

class_cached!(class_nspasteboard, "NSPasteboard");
class_cached!(class_nsdata, "NSData");
class_cached!(class_nsstring, "NSString");

// ---------------------------------------------------------------------------
// NSPasteboard singleton.
// ---------------------------------------------------------------------------

/// Returns `[NSPasteboard generalPasteboard]`.
unsafe fn general_pasteboard() -> Id {
    // SAFETY: `class_nspasteboard()` returns the NSPasteboard class pointer.
    // `sel_general_pasteboard()` is the correct class-method selector.
    // The result is an autoreleased singleton — do not release it.
    unsafe { msg0::<Id>(class_nspasteboard(), sel_general_pasteboard()) }
}

// ---------------------------------------------------------------------------
// NSString helpers.
// ---------------------------------------------------------------------------

/// Construct an `NSString` from a Rust `&str` via `stringWithUTF8String:`.
///
/// Returns `nil` on allocation failure (extremely rare). The returned object
/// is autoreleased; its lifetime is tied to the current autorelease pool.
/// For our use (immediate argument to another ObjC call) this is safe.
unsafe fn nsstring_from_str(s: &str) -> Id {
    let cstr = CString::new(s).expect("NUL byte in clipboard type string");
    // SAFETY: `cstr.as_ptr()` is a valid NUL-terminated C string. The class
    // method `stringWithUTF8String:` copies the bytes internally.
    unsafe {
        msg1::<*const c_char, Id>(
            class_nsstring(),
            sel_string_with_utf8_string(),
            cstr.as_ptr(),
        )
    }
}

/// Convert an `NSString` to a Rust `String`, returning `None` on nil or
/// invalid UTF-8.
unsafe fn nsstring_to_string(s: Id) -> Option<String> {
    if s.is_null() {
        return None;
    }
    // SAFETY: `s` is a non-null NSString. `UTF8String` returns a C string
    // whose lifetime is tied to `s` (and the autorelease pool). We copy the
    // bytes into a Rust String before the pool can drain.
    let utf8: *const c_char = unsafe { msg0::<*const c_char>(s, sel_utf8_string()) };
    if utf8.is_null() {
        return None;
    }
    // SAFETY: `utf8` is non-null and points to a valid NUL-terminated C string
    // owned by `s`. We copy via `to_str().map(String::from)` before returning.
    unsafe { CStr::from_ptr(utf8) }
        .to_str()
        .ok()
        .map(String::from)
}

// ---------------------------------------------------------------------------
// NSData helpers.
// ---------------------------------------------------------------------------

/// Construct an `NSData` object from a Rust byte slice via
/// `dataWithBytes:length:`. The returned object is autoreleased.
unsafe fn nsdata_from_bytes(bytes: &[u8]) -> Id {
    // SAFETY: `bytes.as_ptr()` is valid for `bytes.len()` readable bytes.
    // NSData copies the bytes internally; the slice can be freed after the call.
    unsafe {
        msg2::<*const c_void, NSUInteger, Id>(
            class_nsdata(),
            sel_data_with_bytes_length(),
            bytes.as_ptr().cast(),
            bytes.len(),
        )
    }
}

/// Copy the bytes of an `NSData` object into a `Vec<u8>`.
unsafe fn nsdata_to_vec(data: Id) -> Vec<u8> {
    // SAFETY: `data` is a non-null NSData. `length` and `bytes` are safe
    // ObjC accessors; we copy the slice before the object can be released.
    let len: NSUInteger = unsafe { msg0(data, sel_length()) };
    let ptr: *const c_void = unsafe { msg0(data, sel_bytes()) };
    if ptr.is_null() || len == 0 {
        return Vec::new();
    }
    // SAFETY: `ptr` is valid for `len` readable bytes owned by `data`.
    // `slice::from_raw_parts` is safe here; `to_vec()` copies immediately.
    let slice = unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), len) };
    slice.to_vec()
}

// ---------------------------------------------------------------------------
// UTI / MimeType mapping.
// ---------------------------------------------------------------------------

/// Map a `MimeType` to the NSPasteboard type string (UTI or custom).
///
/// Returns `None` for types that cannot be expressed on macOS (none currently;
/// `Custom(s)` passes through verbatim).
fn mime_to_uti(mime: &MimeType) -> Option<String> {
    match mime {
        MimeType::Text => Some("public.utf8-plain-text".into()),
        MimeType::Html => Some("public.html".into()),
        MimeType::Rtf => Some("public.rtf".into()),
        MimeType::UriList => Some("text/uri-list".into()),
        MimeType::Png => Some("public.png".into()),
        MimeType::Custom(s) => Some(s.clone()),
        // `#[non_exhaustive]` — unknown future variants added in other crates.
        #[allow(unreachable_patterns)]
        _ => None,
    }
}

/// Map a UTI/type-string back to a `MimeType`.
///
/// Returns `None` for unknown types to avoid polluting `available()` with
/// macOS-internal type strings that callers cannot act on.
fn uti_to_mime(name: &str) -> Option<MimeType> {
    match name {
        "public.utf8-plain-text" | "NSStringPboardType" => Some(MimeType::Text),
        "public.html" => Some(MimeType::Html),
        "public.rtf" => Some(MimeType::Rtf),
        "text/uri-list" => Some(MimeType::UriList),
        "public.png" => Some(MimeType::Png),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Backend impl.
// ---------------------------------------------------------------------------

pub(crate) struct MacosBackend;

impl MacosBackend {
    pub(crate) fn new() -> Self {
        Self
    }
}

impl Backend for MacosBackend {
    fn set(&self, sel: Selection, mime: MimeType, bytes: &[u8]) -> Result<(), ClipboardError> {
        // macOS has no primary selection concept.
        if sel != Selection::Clipboard {
            return Err(ClipboardError::UnsupportedMime);
        }
        let uti = mime_to_uti(&mime).ok_or(ClipboardError::UnsupportedMime)?;
        // SAFETY: all ObjC calls below operate on autoreleased objects returned
        // from valid class methods. `general_pasteboard()` returns the process-
        // wide singleton; `clearContents` + `setData:forType:` are the
        // documented write path per Apple developer documentation. The `ok`
        // return from `setData:forType:` is BOOL (mapped to bool here).
        unsafe {
            let pb = general_pasteboard();
            if pb.is_null() {
                return Err(ClipboardError::Io(std::io::Error::other(
                    "generalPasteboard returned nil",
                )));
            }
            // `clearContents` must be called before any setData:forType: per
            // Apple docs. Returns NSInteger (change count); we discard it.
            let _change: isize = msg0(pb, sel_clear_contents());
            let data = nsdata_from_bytes(bytes);
            let ty = nsstring_from_str(&uti);
            let ok: bool = msg2(pb, sel_set_data_for_type(), data, ty);
            if !ok {
                return Err(ClipboardError::Io(std::io::Error::other(
                    "setData:forType: returned NO",
                )));
            }
        }
        Ok(())
    }

    fn get(&self, sel: Selection, mime: MimeType) -> Result<Vec<u8>, ClipboardError> {
        // macOS has no primary selection concept.
        if sel != Selection::Clipboard {
            return Err(ClipboardError::UnsupportedMime);
        }
        let uti = mime_to_uti(&mime).ok_or(ClipboardError::UnsupportedMime)?;
        // SAFETY: `general_pasteboard()` returns the process-wide singleton.
        // `dataForType:` returns an autoreleased NSData (or nil if absent).
        // We copy its bytes immediately via `nsdata_to_vec` before any pool
        // drain can occur.
        unsafe {
            let pb = general_pasteboard();
            if pb.is_null() {
                return Err(ClipboardError::Io(std::io::Error::other(
                    "generalPasteboard returned nil",
                )));
            }
            let ty = nsstring_from_str(&uti);
            let data: Id = msg1(pb, sel_data_for_type(), ty);
            if data.is_null() {
                return Err(ClipboardError::UnsupportedMime);
            }
            Ok(nsdata_to_vec(data))
        }
    }

    fn clear(&self, sel: Selection) -> Result<(), ClipboardError> {
        // macOS has no primary selection concept.
        if sel != Selection::Clipboard {
            return Err(ClipboardError::UnsupportedMime);
        }
        // SAFETY: `clearContents` is the documented way to clear NSPasteboard.
        // Returns NSInteger (change count); we discard it.
        unsafe {
            let pb = general_pasteboard();
            if pb.is_null() {
                return Err(ClipboardError::Io(std::io::Error::other(
                    "generalPasteboard returned nil",
                )));
            }
            let _change: isize = msg0(pb, sel_clear_contents());
        }
        Ok(())
    }

    fn available(&self, sel: Selection) -> Result<Vec<MimeType>, ClipboardError> {
        // Primary selection does not exist on macOS; return empty consistent
        // with the Windows backend convention.
        if sel != Selection::Clipboard {
            return Ok(vec![]);
        }
        // SAFETY: `types` returns an autoreleased NSArray<NSString*> (or nil).
        // We iterate via `count` + `objectAtIndex:`, converting each element
        // with `nsstring_to_string`. All objects are autoreleased and valid for
        // the duration of the loop (no explicit pool drain between calls).
        unsafe {
            let pb = general_pasteboard();
            if pb.is_null() {
                return Ok(vec![]);
            }
            let types: Id = msg0(pb, sel_types());
            if types.is_null() {
                return Ok(vec![]);
            }
            let count: NSUInteger = msg0(types, sel_count());
            let mut out: Vec<MimeType> = Vec::new();
            for i in 0..count {
                let s: Id = msg1(types, sel_object_at_index(), i);
                let Some(name) = nsstring_to_string(s) else {
                    continue;
                };
                if let Some(mime) = uti_to_mime(&name) && !out.contains(&mime) {
                    out.push(mime);
                }
            }
            Ok(out)
        }
    }
}
