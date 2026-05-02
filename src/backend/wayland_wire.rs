//! Wayland wire-protocol primitives.
//!
//! Hand-rolled encode/decode of the Wayland binary protocol over a Unix socket.
//! No libwayland-client involved — only ~10 message types are needed.
//!
//! Wire format: 32-bit aligned, little-endian integers.
//! - Header: u32 object_id | u16 opcode | u16 total_size (8 bytes)
//! - u32/i32/new_id/object: 4 bytes LE
//! - string: u32 len-including-NUL + bytes + NUL + zero-pad to 4-byte boundary
//! - array: u32 len + bytes + zero-pad to 4-byte boundary (no NUL)
//! - fd: out-of-band via SCM_RIGHTS; NOT in the message body

// ---------------------------------------------------------------------------
// Message header
// ---------------------------------------------------------------------------

/// Decoded Wayland message header.
///
/// Every message starts with an 8-byte header: object_id (4), then a packed
/// u32 that encodes opcode in the low 16 bits and total size in the high 16.
/// We store them separated for clarity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MessageHeader {
    /// Target object this message is addressed to.
    pub object_id: u32,
    /// Method index within the object's interface.
    pub opcode: u16,
    /// Total message size in bytes, header included.
    pub size: u16,
}

// ---------------------------------------------------------------------------
// Encode helpers
// ---------------------------------------------------------------------------

/// Append a u32 in little-endian byte order.
pub(crate) fn encode_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// Append a Wayland wire-protocol string.
///
/// Format: u32 length (bytes of `s` + 1 for NUL) + raw bytes + NUL byte +
/// zero-padding so the total is a multiple of 4.
pub(crate) fn encode_string(out: &mut Vec<u8>, s: &str) {
    let len_with_nul = s.len() + 1; // includes the trailing NUL
    encode_u32(out, len_with_nul as u32);
    out.extend_from_slice(s.as_bytes());
    out.push(0); // NUL terminator
    let pad = pad4(len_with_nul);
    out.extend(std::iter::repeat_n(0u8, pad));
}

/// Build a complete Wayland message: header + pre-encoded args bytes.
///
/// `args` must already be encoded (via `encode_u32`, `encode_string`, etc).
/// The total message size is 8 (header) + args.len(), rounded — but Wayland
/// size field equals the exact byte count, which must be a multiple of 4.
/// Callers must ensure `args` is already 4-byte aligned (it always is when
/// built from our encode helpers).
pub(crate) fn encode_message(object_id: u32, opcode: u16, args: &[u8]) -> Vec<u8> {
    let total: u16 = (8 + args.len()) as u16; // header (8) + args
    let size_opcode: u32 = (u32::from(total) << 16) | u32::from(opcode);

    let mut out = Vec::with_capacity(8 + args.len());
    encode_u32(&mut out, object_id);
    encode_u32(&mut out, size_opcode);
    out.extend_from_slice(args);
    out
}

// ---------------------------------------------------------------------------
// Decode helpers
// ---------------------------------------------------------------------------

/// Parse a Wayland message header from `buf`.
///
/// Returns `None` if fewer than 8 bytes are available (header is incomplete).
/// On success returns the decoded `MessageHeader` and the remaining bytes
/// (i.e. the args slice that follows, length = header.size - 8).
///
/// Caller must check that `buf.len() >= header.size` before consuming the rest.
pub(crate) fn parse_message_header(buf: &[u8]) -> Option<(MessageHeader, &[u8])> {
    if buf.len() < 8 {
        return None;
    }
    let object_id = u32::from_le_bytes(buf[0..4].try_into().ok()?);
    let size_opcode = u32::from_le_bytes(buf[4..8].try_into().ok()?);
    let opcode = (size_opcode & 0xffff) as u16;
    let size = (size_opcode >> 16) as u16;
    let hdr = MessageHeader {
        object_id,
        opcode,
        size,
    };
    Some((hdr, &buf[8..]))
}

/// Parse a Wayland wire-protocol string from `args`.
///
/// Consumes: u32 len-field + len bytes (including NUL) + padding to 4.
/// Returns `(&str, remaining_args)` on success, `None` on truncation or
/// invalid UTF-8.
pub(crate) fn parse_string(args: &[u8]) -> Option<(&str, &[u8])> {
    if args.len() < 4 {
        return None;
    }
    let len_with_nul = u32::from_le_bytes(args[0..4].try_into().ok()?) as usize;
    if len_with_nul == 0 {
        // Empty string: len=1 (NUL only); len=0 would be malformed, treat as empty.
        let total_consumed = 4; // just the length field; nothing after
        return Some(("", &args[total_consumed..]));
    }
    let payload_start = 4;
    let payload_end = payload_start + len_with_nul;
    if args.len() < payload_end {
        return None;
    }
    // Exclude the trailing NUL from the str slice.
    let str_bytes = &args[payload_start..payload_end - 1];
    let s = std::str::from_utf8(str_bytes).ok()?;
    // Advance past payload + padding to 4-byte boundary.
    let padded_len = pad4_up(len_with_nul);
    let total_consumed = 4 + padded_len;
    if args.len() < total_consumed {
        return None;
    }
    Some((s, &args[total_consumed..]))
}

/// Parse a u32 from `args` (little-endian).
pub(crate) fn parse_u32(args: &[u8]) -> Option<(u32, &[u8])> {
    if args.len() < 4 {
        return None;
    }
    let v = u32::from_le_bytes(args[0..4].try_into().ok()?);
    Some((v, &args[4..]))
}

// ---------------------------------------------------------------------------
// Padding helpers
// ---------------------------------------------------------------------------

/// Number of zero-pad bytes needed to round `len` up to next 4-byte boundary.
///
/// Returns 0 when `len` is already aligned.
fn pad4(len: usize) -> usize {
    let rem = len % 4;
    if rem == 0 { 0 } else { 4 - rem }
}

/// Round `len` up to the nearest 4-byte boundary.
fn pad4_up(len: usize) -> usize {
    len.div_ceil(4) * 4
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // 1. u32 round-trip: encode then parse must reproduce original value.
    #[test]
    fn encode_decode_u32_round_trip() {
        let cases: &[u32] = &[0, 1, 0x0000_FFFF, 0xDEAD_BEEF, u32::MAX];
        for &v in cases {
            let mut buf = Vec::new();
            encode_u32(&mut buf, v);
            assert_eq!(buf.len(), 4, "u32 must encode to exactly 4 bytes");
            let (decoded, rest) = parse_u32(&buf).expect("parse_u32 failed");
            assert_eq!(decoded, v, "round-trip mismatch for {v:#010x}");
            assert!(rest.is_empty());
        }
    }

    // 2. string round-trip — all four pad residues (0, 1, 2, 3 chars mod 4
    //    after the NUL), plus empty string.
    #[test]
    fn encode_decode_string_round_trip() {
        // Each string's length (including the length-field u32) must be a
        // multiple of 4 bytes after encoding.
        let cases: &[&str] = &[
            "",     // len_with_nul=1 → pad 3 → total wire = 4+1+3=8
            "a",    // len_with_nul=2 → pad 2 → total wire = 4+2+2=8
            "ab",   // len_with_nul=3 → pad 1 → total wire = 4+3+1=8
            "abc",  // len_with_nul=4 → pad 0 → total wire = 4+4+0=8
            "abcd", // len_with_nul=5 → pad 3 → total wire = 4+5+3=12
            "wl_registry",
            "ext_data_control_manager_v1",
        ];

        for &s in cases {
            let mut buf = Vec::new();
            encode_string(&mut buf, s);

            // Wire bytes must be a multiple of 4.
            assert_eq!(buf.len() % 4, 0, "encoded string not 4-byte aligned: {s:?}");

            let (decoded, rest) = parse_string(&buf).expect("parse_string failed");
            assert_eq!(decoded, s, "round-trip mismatch for {s:?}");
            assert!(rest.is_empty(), "unexpected trailing bytes for {s:?}");
        }
    }

    // 3. encode_message layout — verify byte-by-byte for known input.
    //
    // Test: object_id=1, opcode=1 (get_registry), args = u32(2) as new_id.
    // Expected bytes:
    //   [01 00 00 00]  object_id = 1 LE
    //   [0c 00 01 00]  size=12 in high 16, opcode=1 in low 16
    //                  packed as u32 LE: (12<<16)|1 = 0x000C_0001
    //                  bytes: 01 00 0C 00
    //   [02 00 00 00]  new_id = 2 LE
    #[test]
    fn encode_message_header_layout() {
        let mut args = Vec::new();
        encode_u32(&mut args, 2u32); // new_id = 2

        let msg = encode_message(1, 1, &args);
        assert_eq!(msg.len(), 12);

        // object_id = 1 LE
        assert_eq!(&msg[0..4], &[0x01, 0x00, 0x00, 0x00]);
        // size=12 high 16, opcode=1 low 16 — packed u32 LE
        // u32 = (12 << 16) | 1 = 0x000C_0001
        // LE bytes: 0x01, 0x00, 0x0C, 0x00
        assert_eq!(&msg[4..8], &[0x01, 0x00, 0x0C, 0x00]);
        // args: new_id=2
        assert_eq!(&msg[8..12], &[0x02, 0x00, 0x00, 0x00]);

        // Cross-check with parse_message_header.
        let (hdr, rest) = parse_message_header(&msg).expect("header parse failed");
        assert_eq!(hdr.object_id, 1);
        assert_eq!(hdr.opcode, 1);
        assert_eq!(hdr.size, 12);
        assert_eq!(rest, &[0x02u8, 0x00, 0x00, 0x00]);
    }

    // 4. parse_string consumes correct trailing pad bytes and leaves remainder.
    #[test]
    fn parse_string_padding() {
        // Build: string "ab" then u32(99) sentinel to verify remainder.
        // "ab" encodes as: [03 00 00 00] (len=3) + [61 62 00] + [00] (pad) = 8 bytes
        let mut buf = Vec::new();
        encode_string(&mut buf, "ab");
        encode_u32(&mut buf, 99);

        let (s, rest) = parse_string(&buf).expect("parse failed");
        assert_eq!(s, "ab");

        let (sentinel, rest2) = parse_u32(rest).expect("sentinel parse failed");
        assert_eq!(sentinel, 99);
        assert!(rest2.is_empty());
    }

    // 5. parse_message_header returns None when buffer is too short.
    #[test]
    fn parse_message_header_partial() {
        // Fewer than 8 bytes.
        assert!(parse_message_header(&[]).is_none());
        assert!(parse_message_header(&[1, 0, 0, 0]).is_none());
        assert!(parse_message_header(&[1, 0, 0, 0, 0, 0, 0]).is_none());
        // Exactly 8 bytes — must succeed.
        let full = encode_message(5, 3, &[]);
        assert_eq!(full.len(), 8);
        let r = parse_message_header(&full);
        assert!(r.is_some());
        let (hdr, _) = r.unwrap();
        assert_eq!(hdr.object_id, 5);
        assert_eq!(hdr.opcode, 3);
        assert_eq!(hdr.size, 8);
    }

    // 6. Pad bytes must be zero-filled in string encoding.
    #[test]
    fn padding_zero_filled() {
        // "a" → len_with_nul=2 → 2 pad bytes must be 0x00.
        let mut buf = Vec::new();
        encode_string(&mut buf, "a");
        // buf: [02 00 00 00] [61] [00] [00 00]
        //       len            a    NUL   pad
        assert_eq!(buf.len(), 8);
        assert_eq!(buf[6], 0x00, "first pad byte must be zero");
        assert_eq!(buf[7], 0x00, "second pad byte must be zero");
    }

    // Bonus: pad4 internal helper correctness.
    #[test]
    fn pad4_helper() {
        assert_eq!(pad4(0), 0);
        assert_eq!(pad4(1), 3);
        assert_eq!(pad4(2), 2);
        assert_eq!(pad4(3), 1);
        assert_eq!(pad4(4), 0);
        assert_eq!(pad4(5), 3);

        assert_eq!(pad4_up(0), 0);
        assert_eq!(pad4_up(1), 4);
        assert_eq!(pad4_up(4), 4);
        assert_eq!(pad4_up(5), 8);
    }
}
