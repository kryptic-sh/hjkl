//! Windows `CF_HDROP` / `DROPFILES` structure builder and parser.
//!
//! Used for `text/uri-list` on Windows. Handles UNC path mapping:
//! `\\server\share\foo` ↔ `file://server/share/foo`.
//!
//! This module is **not** cfg-gated so that the pure-Rust `build`/`parse`
//! functions can be unit-tested on any host platform (including Linux CI).
//!
//! # DROPFILES layout (Win32)
//!
//! ```text
//! offset  0: pFiles (DWORD / u32 LE) = 20   ← byte offset of the file list
//! offset  4: pt.x   (LONG  / i32 LE) = 0
//! offset  8: pt.y   (LONG  / i32 LE) = 0
//! offset 12: fNC    (BOOL  / i32 LE) = 0
//! offset 16: fWide  (BOOL  / i32 LE) = 1    ← paths are UTF-16 LE
//! offset 20: <UTF-16 LE paths, each null-terminated>
//!            <extra u16 null terminator after the last path>
//! ```
//!
//! Total header: 20 bytes (5 × 4-byte fields).

use std::io;
use std::path::{Path, PathBuf};

use crate::ClipboardError;

/// Byte length of the DROPFILES header.
const HEADER_LEN: usize = 20;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Build a CF_HDROP byte payload from a slice of native paths.
///
/// Each path is encoded as a null-terminated UTF-16 LE string. After all
/// paths there is an extra null (u16 zero) as the list terminator.
///
/// On Linux this function round-trips any UTF-8 path string through UTF-16.
/// The caller (always the Windows backend in production) is responsible for
/// passing valid Windows-style absolute paths.
///
/// # Errors
///
/// Returns [`ClipboardError::InvalidUri`] if any path is:
/// - Empty.
/// - Contains an interior null character (would collide with the UTF-16 null
///   terminator and corrupt the list).
pub(crate) fn build(paths: &[&Path]) -> Result<Vec<u8>, ClipboardError> {
    // Collect all the UTF-16 path strings up-front so we can size the buffer.
    let mut encoded: Vec<Vec<u16>> = Vec::with_capacity(paths.len());
    for &path in paths {
        let s = path.to_str().ok_or(ClipboardError::InvalidUri)?;
        if s.is_empty() {
            return Err(ClipboardError::InvalidUri);
        }
        // Reject interior nulls — they would break the double-null terminator
        // convention and corrupt any subsequent paths.
        if s.contains('\0') {
            return Err(ClipboardError::InvalidUri);
        }
        // Encode as UTF-16 with an explicit null terminator code unit.
        let units: Vec<u16> = s.encode_utf16().chain(std::iter::once(0u16)).collect();
        encoded.push(units);
    }

    // Total size: header + all UTF-16 units (each path already has its own
    // null) + one extra null u16 as the list terminator.
    let total_utf16_units: usize = encoded.iter().map(|v| v.len()).sum::<usize>() + 1;
    let total_bytes = HEADER_LEN + total_utf16_units * 2;

    let mut out = Vec::with_capacity(total_bytes);

    // --- DROPFILES header (all LE) ---
    // pFiles = 20 (byte offset of the file list).
    out.extend_from_slice(&20u32.to_le_bytes());
    // pt.x = 0.
    out.extend_from_slice(&0i32.to_le_bytes());
    // pt.y = 0.
    out.extend_from_slice(&0i32.to_le_bytes());
    // fNC = 0.
    out.extend_from_slice(&0i32.to_le_bytes());
    // fWide = 1 (wide / UTF-16 paths).
    out.extend_from_slice(&1i32.to_le_bytes());

    debug_assert_eq!(out.len(), HEADER_LEN, "header must be exactly 20 bytes");

    // --- UTF-16 LE path list ---
    for units in &encoded {
        for &unit in units {
            out.extend_from_slice(&unit.to_le_bytes());
        }
    }

    // Extra null terminator (u16 zero) marking end of list.
    out.extend_from_slice(&0u16.to_le_bytes());

    Ok(out)
}

/// Parse a CF_HDROP byte payload into a list of paths.
///
/// # Errors
///
/// Returns `ClipboardError::Io(other("malformed CF_HDROP"))` if:
/// - The payload is shorter than 20 bytes (header too short).
/// - The `pFiles` offset is out of bounds.
/// - `fWide == 0` (ANSI CF_HDROP — modern Windows always uses wide).
/// - The UTF-16 sequence is invalid or the terminating double-null is missing.
pub(crate) fn parse(bytes: &[u8]) -> Result<Vec<PathBuf>, ClipboardError> {
    let bad = || ClipboardError::Io(io::Error::other("malformed CF_HDROP"));

    if bytes.len() < HEADER_LEN {
        return Err(bad());
    }

    // Read pFiles (u32 LE at offset 0).
    let p_files = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
    if p_files > bytes.len() {
        return Err(bad());
    }

    // Read fWide (i32 LE at offset 16).
    let f_wide = i32::from_le_bytes(bytes[16..20].try_into().unwrap());
    if f_wide == 0 {
        return Err(bad());
    }

    // The file list starts at byte offset `p_files`.
    let list_bytes = &bytes[p_files..];

    // The list is a sequence of null-terminated UTF-16 LE strings, terminated
    // by an extra null u16.
    if !list_bytes.len().is_multiple_of(2) {
        return Err(bad());
    }

    let units: Vec<u16> = list_bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();

    // There must be at least one u16 for the terminating double-null.
    if units.is_empty() {
        return Err(bad());
    }

    let mut paths = Vec::new();
    let mut pos = 0;

    loop {
        if pos >= units.len() {
            // Ran off the end without finding the list terminator.
            return Err(bad());
        }

        // A u16 null at the current position with nothing in the current path
        // means we hit the double-null list terminator.
        if units[pos] == 0 {
            break;
        }

        // Find the null terminator for this path.
        let end = units[pos..]
            .iter()
            .position(|&u| u == 0)
            .map(|rel| pos + rel)
            .ok_or_else(bad)?;

        let path_units = &units[pos..end];
        let path_str = String::from_utf16(path_units).map_err(|_| bad())?;
        paths.push(PathBuf::from(path_str));

        pos = end + 1; // skip past the null terminator
    }

    Ok(paths)
}

// ---------------------------------------------------------------------------
// Tests (run on any platform — pure Rust, no Win32)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // Helper: build → parse round-trip assertion.
    fn round_trip(path_strs: &[&str]) {
        let paths: Vec<&Path> = path_strs.iter().map(Path::new).collect();
        let bytes = build(&paths).expect("build failed");
        let recovered = parse(&bytes).expect("parse failed");
        let recovered_strs: Vec<&str> = recovered.iter().map(|p| p.to_str().unwrap()).collect();
        assert_eq!(
            recovered_strs, path_strs,
            "round-trip mismatch for {path_strs:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Round-trip tests
    // -----------------------------------------------------------------------

    #[test]
    fn round_trip_single_drive_letter() {
        round_trip(&["C:\\foo\\bar.txt"]);
    }

    #[test]
    fn round_trip_single_unc() {
        round_trip(&["\\\\server\\share\\file.txt"]);
    }

    #[test]
    fn round_trip_multiple_mixed() {
        round_trip(&[
            "C:\\foo\\bar.txt",
            "\\\\server\\share\\file.txt",
            "D:\\Program Files\\app.exe",
        ]);
    }

    #[test]
    fn round_trip_path_with_spaces() {
        round_trip(&["D:\\Program Files\\app.exe"]);
    }

    #[test]
    fn round_trip_non_ascii() {
        // UTF-16 round-trip: "café" and "naïve" survive the u16 encoding.
        round_trip(&["E:\\café\\naïve.txt"]);
    }

    // -----------------------------------------------------------------------
    // Byte-level layout verification (single path "C:\foo")
    // -----------------------------------------------------------------------

    #[test]
    fn byte_layout_header_fields() {
        let bytes = build(&[Path::new("C:\\foo")]).unwrap();

        // offset 0: pFiles (u32 LE) = 20
        let p_files = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
        assert_eq!(p_files, 20, "pFiles must be 20");

        // offset 4: pt.x (i32 LE) = 0
        let pt_x = i32::from_le_bytes(bytes[4..8].try_into().unwrap());
        assert_eq!(pt_x, 0, "pt.x must be 0");

        // offset 8: pt.y (i32 LE) = 0
        let pt_y = i32::from_le_bytes(bytes[8..12].try_into().unwrap());
        assert_eq!(pt_y, 0, "pt.y must be 0");

        // offset 12: fNC (i32 LE) = 0
        let f_nc = i32::from_le_bytes(bytes[12..16].try_into().unwrap());
        assert_eq!(f_nc, 0, "fNC must be 0");

        // offset 16: fWide (i32 LE) = 1
        let f_wide = i32::from_le_bytes(bytes[16..20].try_into().unwrap());
        assert_eq!(f_wide, 1, "fWide must be 1");
    }

    #[test]
    fn byte_layout_utf16_content() {
        // "A" is a simple single-unit UTF-16 code point (0x0041).
        let bytes = build(&[Path::new("A")]).unwrap();

        // offset 20: 'A' in UTF-16 LE = 0x41 0x00
        assert_eq!(bytes[20], 0x41, "first byte of 'A' in UTF-16 LE");
        assert_eq!(bytes[21], 0x00, "second byte of 'A' in UTF-16 LE");

        // offset 22: null terminator for the path = 0x00 0x00
        assert_eq!(bytes[22], 0x00);
        assert_eq!(bytes[23], 0x00);

        // offset 24: list terminator = 0x00 0x00
        assert_eq!(bytes[24], 0x00);
        assert_eq!(bytes[25], 0x00);

        assert_eq!(bytes.len(), 26, "total length for single 'A' path");
    }

    // -----------------------------------------------------------------------
    // build error cases
    // -----------------------------------------------------------------------

    #[test]
    fn build_rejects_empty_path() {
        let result = build(&[Path::new("")]);
        assert!(result.is_err(), "build should reject empty path");
    }

    #[test]
    fn build_rejects_interior_null() {
        // Path containing a null byte in its string representation.
        // Note: on most OSes Path::new("foo\0bar") is representable as a
        // Rust Path (it stores OsStr), but to_str() will return Some because
        // the bytes happen to be valid UTF-8 with an embedded null.
        // We detect the interior null explicitly.
        let s = "foo\0bar";
        let result = build(&[Path::new(s)]);
        assert!(
            result.is_err(),
            "build should reject path with interior null"
        );
    }

    // -----------------------------------------------------------------------
    // parse error cases
    // -----------------------------------------------------------------------

    #[test]
    fn parse_rejects_too_short() {
        let short = vec![0u8; 10]; // less than 20 bytes
        assert!(parse(&short).is_err(), "parse should reject < 20 bytes");
    }

    #[test]
    fn parse_rejects_f_wide_zero() {
        // Build a valid header but set fWide = 0.
        let mut header = vec![0u8; 20];
        // pFiles = 20
        header[0..4].copy_from_slice(&20u32.to_le_bytes());
        // fWide = 0 (bytes 16..20 already zero from vec![0u8; 20])
        // Add a minimal null terminator.
        header.extend_from_slice(&[0u8, 0u8]);
        assert!(
            parse(&header).is_err(),
            "parse should reject fWide == 0 (ANSI)"
        );
    }

    #[test]
    fn parse_rejects_offset_out_of_bounds() {
        let mut header = vec![0u8; 20];
        // pFiles = 9999 (way beyond the payload)
        header[0..4].copy_from_slice(&9999u32.to_le_bytes());
        // fWide = 1
        header[16..20].copy_from_slice(&1i32.to_le_bytes());
        assert!(
            parse(&header).is_err(),
            "parse should reject pFiles out of bounds"
        );
    }

    #[test]
    fn parse_rejects_missing_list_terminator() {
        // Build a valid header with fWide=1 and a path "A" but NO list
        // terminator (only the path's own null, no extra null after).
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&20u32.to_le_bytes()); // pFiles
        bytes.extend_from_slice(&0i32.to_le_bytes()); // pt.x
        bytes.extend_from_slice(&0i32.to_le_bytes()); // pt.y
        bytes.extend_from_slice(&0i32.to_le_bytes()); // fNC
        bytes.extend_from_slice(&1i32.to_le_bytes()); // fWide
        // Path "A\0" — the path's null terminator serves as the "list
        // terminator" as well (this is actually valid for a single path
        // where the path null IS the double-null). Let's test with a path
        // that has NO null at all to trigger the missing-terminator error.
        // 'A' in UTF-16 LE, no null.
        bytes.extend_from_slice(&[0x41u8, 0x00u8]);
        // No terminating null after — parse must error.
        assert!(
            parse(&bytes).is_err(),
            "parse should error on missing list terminator"
        );
    }
}
