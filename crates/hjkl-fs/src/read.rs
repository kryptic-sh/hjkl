//! Bounded reads.
//!
//! Every read of untrusted or unbounded input goes through here with an explicit
//! cap. hjkl already had caps at each call site (swap header 1 MiB, undo 256 MiB,
//! body 64 MiB, LSP message 16 MiB, stdin 256 MiB), but hand-rolled per site —
//! and one of them was missed, which is how `hjkl -` could be pointed at
//! `/dev/zero` and allocate until OOM (security finding H3). Centralizing the
//! cap makes "unbounded read" a thing you have to write deliberately rather than
//! forget accidentally.
//!
//! Reads are capped, not truncated: exceeding the limit is an error, because
//! silently loading a prefix of a file into an editor is worse than refusing it.

use std::fs::File;
use std::io;
use std::io::Read;
use std::path::Path;

/// Cap presets, so call sites name an intent rather than repeating a magic
/// number. These mirror the limits the individual paths already enforced.
pub mod caps {
    /// Swap file header (1 MiB).
    pub const SWAP_HEADER: u64 = 1024 * 1024;
    /// Serialized undo history (256 MiB).
    pub const UNDO: u64 = 256 * 1024 * 1024;
    /// Buffer body / formatter I/O (64 MiB).
    pub const BODY: u64 = 64 * 1024 * 1024;
    /// A single LSP message body (16 MiB).
    pub const LSP_MESSAGE: u64 = 16 * 1024 * 1024;
    /// Anything piped in on stdin (256 MiB).
    pub const STDIN: u64 = 256 * 1024 * 1024;
}

/// The error a read produces when it would exceed its cap.
fn too_large(limit: u64) -> io::Error {
    io::Error::new(
        io::ErrorKind::FileTooLarge,
        format!("input exceeds {limit} byte limit"),
    )
}

/// Read at most `limit` bytes from `reader`, erroring if there are more.
///
/// Reads `limit + 1` bytes: if the extra byte arrives the input is over the cap.
/// That distinguishes "exactly at the limit" from "over it" without reading the
/// whole stream, which is the point on a source that never ends.
pub fn read_capped_from<R: Read>(mut reader: R, limit: u64) -> io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    let mut taken = reader.by_ref().take(limit.saturating_add(1));
    taken.read_to_end(&mut buf)?;
    if buf.len() as u64 > limit {
        return Err(too_large(limit));
    }
    Ok(buf)
}

/// Read at most `limit` bytes from `reader` as UTF-8.
pub fn read_to_string_capped_from<R: Read>(reader: R, limit: u64) -> io::Result<String> {
    let bytes = read_capped_from(reader, limit)?;
    String::from_utf8(bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Read `path`, erroring if it holds more than `limit` bytes.
///
/// The file's declared length is checked first so an oversized file is rejected
/// without reading it, then the cap is enforced again during the read — the
/// length can be a lie on `/proc` and friends, or change under us.
pub fn read_capped(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let file = File::open(path)?;
    if let Ok(meta) = file.metadata()
        && meta.len() > limit
    {
        return Err(too_large(limit));
    }
    read_capped_from(file, limit)
}

/// Read `path` as UTF-8, erroring if it holds more than `limit` bytes.
pub fn read_to_string_capped(path: &Path, limit: u64) -> io::Result<String> {
    let bytes = read_capped(path, limit)?;
    String::from_utf8(bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Read `path` as UTF-8 with **no size limit**.
///
/// For content the user explicitly asked for by name — opening a buffer. A cap
/// here is not a safety bound, it is a refusal to do the thing the user asked
/// for, and an editor that will not open a large file the previous version
/// opened has regressed.
///
/// This exists so "unbounded" is a deliberate, greppable choice routed through
/// the same seam, rather than a bare `std::fs::read_to_string` that silently
/// escapes it. Use [`read_to_string_capped`] for anything a *remote* or
/// *untrusted* party controls the size of — an LSP message, a pipe, a swap file
/// header.
pub fn read_to_string_unbounded(path: &Path) -> io::Result<String> {
    std::fs::read_to_string(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_file_under_cap() {
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("f.bin");
        std::fs::write(&p, b"hello").unwrap();
        assert_eq!(read_capped(&p, 1024).unwrap(), b"hello");
    }

    #[test]
    fn reads_file_exactly_at_cap() {
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("f.bin");
        std::fs::write(&p, b"12345").unwrap();
        // Exactly at the limit is allowed; only *more* is an error.
        assert_eq!(read_capped(&p, 5).unwrap(), b"12345");
    }

    #[test]
    fn rejects_file_over_cap() {
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("f.bin");
        std::fs::write(&p, b"123456").unwrap();
        let err = read_capped(&p, 5).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::FileTooLarge);
    }

    /// The reason the cap exists: a reader that never ends must not be able to
    /// allocate without bound.
    #[test]
    fn rejects_endless_reader_without_hanging() {
        struct Endless;
        impl Read for Endless {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                buf.fill(0);
                Ok(buf.len())
            }
        }
        let err = read_capped_from(Endless, 4096).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::FileTooLarge);
    }

    #[test]
    fn string_read_rejects_invalid_utf8() {
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("f.bin");
        std::fs::write(&p, [0xff, 0xfe]).unwrap();
        let err = read_to_string_capped(&p, 1024).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn missing_file_is_not_found() {
        let td = tempfile::tempdir().unwrap();
        let err = read_capped(&td.path().join("nope"), 16).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }
}
