//! Windows DIB <-> PNG conversion via `miniz_oxide`.
//!
//! Modern apps use the `"PNG"` registered format directly; legacy apps use
//! `CF_DIBV5`. This module converts between the two so both directions work.
//!
//! The clipboard `CF_DIBV5` format is a `BITMAPV5HEADER` (124 bytes) followed
//! immediately by pixel data. There is **no** `BITMAPFILEHEADER` prefix — that
//! prefix is only used in `.bmp` files.
//!
//! Supported PNG colour types: 2 (RGB) and 6 (RGBA), bit depth 8 only.
//!
//! This module is **not** cfg-gated so that the pure-Rust conversion functions
//! can be unit-tested on any host platform (including Linux CI).

use crate::ClipboardError;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// PNG signature bytes (8-byte magic at the start of every PNG file).
const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];

/// Byte size of a `BITMAPV5HEADER`.
const DIB_HEADER_SIZE: u32 = 124;

/// BI_RGB: uncompressed 24 bpp (no explicit masks).
const BI_RGB: u32 = 0;

/// BI_BITFIELDS: explicit channel masks; required for 32 bpp so apps read alpha.
const BI_BITFIELDS: u32 = 3;

/// PNG colour type: RGB (3 channels, no alpha).
const PNG_COLOR_RGB: u8 = 2;

/// PNG colour type: RGBA (4 channels, with alpha).
const PNG_COLOR_RGBA: u8 = 6;

// ---------------------------------------------------------------------------
// CRC32 (IEEE 802.3 polynomial 0xEDB88320)
// ---------------------------------------------------------------------------

/// Precomputed CRC32 table, initialised once.
fn crc32_table() -> &'static [u32; 256] {
    use std::sync::OnceLock;
    static TABLE: OnceLock<[u32; 256]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, slot) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                if c & 1 != 0 {
                    c = 0xEDB8_8320 ^ (c >> 1);
                } else {
                    c >>= 1;
                }
            }
            *slot = c;
        }
        t
    })
}

/// Compute IEEE 802.3 CRC32 over `data`.
fn crc32(data: &[u8]) -> u32 {
    let table = crc32_table();
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc = table[((crc ^ u32::from(byte)) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

// ---------------------------------------------------------------------------
// PNG chunk helpers
// ---------------------------------------------------------------------------

/// Append a PNG chunk to `out`: len(u32 BE) + type(4) + data + crc(u32 BE).
fn write_chunk(out: &mut Vec<u8>, chunk_type: &[u8; 4], data: &[u8]) {
    let len = data.len() as u32;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(chunk_type);
    out.extend_from_slice(data);

    let mut crc_input = Vec::with_capacity(4 + data.len());
    crc_input.extend_from_slice(chunk_type);
    crc_input.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
}

/// Parse the next PNG chunk from `data` starting at `pos`.
///
/// Returns `(chunk_type, chunk_data_slice, new_pos)` or an error.
fn read_chunk(data: &[u8], pos: usize) -> Result<([u8; 4], &[u8], usize), ClipboardError> {
    let bad = |msg: &'static str| ClipboardError::io_other(msg);

    if pos + 8 > data.len() {
        return Err(bad("truncated PNG chunk header"));
    }

    let len = u32::from_be_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
    let type_start = pos + 4;
    let data_start = type_start + 4;
    let data_end = data_start + len;
    let crc_end = data_end + 4;

    if crc_end > data.len() {
        return Err(bad("truncated PNG chunk data"));
    }

    let chunk_type: [u8; 4] = data[type_start..data_start].try_into().unwrap();
    let chunk_data = &data[data_start..data_end];

    // Verify CRC.
    let mut crc_input = Vec::with_capacity(4 + len);
    crc_input.extend_from_slice(&chunk_type);
    crc_input.extend_from_slice(chunk_data);
    let expected = crc32(&crc_input);
    let got = u32::from_be_bytes(data[data_end..crc_end].try_into().unwrap());
    if expected != got {
        return Err(bad("PNG CRC mismatch"));
    }

    Ok((chunk_type, chunk_data, crc_end))
}

// ---------------------------------------------------------------------------
// PNG filter (unfilter only — emit filter type 0 = None when building)
// ---------------------------------------------------------------------------

/// Un-apply one PNG filter row.
///
/// `prev` is the reconstructed pixels of the row above (all zeros for the
/// first row). `bpp` is the number of bytes per pixel (3 for RGB, 4 for RGBA).
fn unfilter_row(filter: u8, row: &mut [u8], prev: &[u8], bpp: usize) -> Result<(), ClipboardError> {
    let bad = || ClipboardError::io_other("unknown PNG filter type");
    match filter {
        // None: no transformation.
        0 => {}
        // Sub: each byte = raw - left.
        1 => {
            for i in bpp..row.len() {
                row[i] = row[i].wrapping_add(row[i - bpp]);
            }
        }
        // Up: each byte = raw + byte_above.
        2 => {
            for i in 0..row.len() {
                row[i] = row[i].wrapping_add(prev[i]);
            }
        }
        // Average: each byte = raw + floor((left + above) / 2).
        3 => {
            for i in 0..row.len() {
                let left: u16 = if i >= bpp { row[i - bpp] as u16 } else { 0 };
                let above: u16 = prev[i] as u16;
                row[i] = row[i].wrapping_add(((left + above) / 2) as u8);
            }
        }
        // Paeth: each byte = raw + paeth_predictor(left, above, upper-left).
        4 => {
            for i in 0..row.len() {
                let left: i32 = if i >= bpp { row[i - bpp] as i32 } else { 0 };
                let above: i32 = prev[i] as i32;
                let upper_left: i32 = if i >= bpp { prev[i - bpp] as i32 } else { 0 };
                row[i] = row[i].wrapping_add(paeth(left, above, upper_left));
            }
        }
        _ => return Err(bad()),
    }
    Ok(())
}

/// Paeth predictor (PNG spec section 9.4).
fn paeth(a: i32, b: i32, c: i32) -> u8 {
    let p = a + b - c;
    let pa = (p - a).abs();
    let pb = (p - b).abs();
    let pc = (p - c).abs();
    if pa <= pb && pa <= pc {
        a as u8
    } else if pb <= pc {
        b as u8
    } else {
        c as u8
    }
}

// ---------------------------------------------------------------------------
// DIB header helpers
// ---------------------------------------------------------------------------

/// Write a `BITMAPV5HEADER` into `out`.
///
/// Always writes exactly 124 bytes. For 32 bpp uses `BI_BITFIELDS` with
/// explicit ARGB masks so apps interpret the alpha channel rather than treating
/// the high byte as padding (XRGB).
fn write_dib_header(out: &mut Vec<u8>, width: u32, height: u32, bpp: u16) {
    let stride = row_stride(width, bpp);
    let image_size = stride * height;

    // bV5Size
    out.extend_from_slice(&DIB_HEADER_SIZE.to_le_bytes());
    // bV5Width
    out.extend_from_slice(&(width as i32).to_le_bytes());
    // bV5Height (positive = bottom-up row order)
    out.extend_from_slice(&(height as i32).to_le_bytes());
    // bV5Planes
    out.extend_from_slice(&1u16.to_le_bytes());
    // bV5BitCount
    out.extend_from_slice(&bpp.to_le_bytes());
    // bV5Compression
    let compression: u32 = if bpp == 32 { BI_BITFIELDS } else { BI_RGB };
    out.extend_from_slice(&compression.to_le_bytes());
    // bV5SizeImage
    out.extend_from_slice(&image_size.to_le_bytes());
    // bV5XPelsPerMeter (~72 DPI)
    out.extend_from_slice(&2835i32.to_le_bytes());
    // bV5YPelsPerMeter (~72 DPI)
    out.extend_from_slice(&2835i32.to_le_bytes());
    // bV5ClrUsed
    out.extend_from_slice(&0u32.to_le_bytes());
    // bV5ClrImportant
    out.extend_from_slice(&0u32.to_le_bytes());

    // Channel masks — only meaningful for BI_BITFIELDS (32 bpp).
    // BI_BITFIELDS so apps interpret 32bpp as ARGB rather than XRGB.
    if bpp == 32 {
        out.extend_from_slice(&0x00FF_0000u32.to_le_bytes()); // bV5RedMask
        out.extend_from_slice(&0x0000_FF00u32.to_le_bytes()); // bV5GreenMask
        out.extend_from_slice(&0x0000_00FFu32.to_le_bytes()); // bV5BlueMask
        out.extend_from_slice(&0xFF00_0000u32.to_le_bytes()); // bV5AlphaMask
    } else {
        out.extend_from_slice(&0u32.to_le_bytes()); // bV5RedMask
        out.extend_from_slice(&0u32.to_le_bytes()); // bV5GreenMask
        out.extend_from_slice(&0u32.to_le_bytes()); // bV5BlueMask
        out.extend_from_slice(&0u32.to_le_bytes()); // bV5AlphaMask
    }

    // bV5CSType: LCS_sRGB ('sRGB' as u32 LE = 0x73524742)
    out.extend_from_slice(&0x7352_4742u32.to_le_bytes());

    // bV5Endpoints (CIEXYZTRIPLE — 36 bytes of zero)
    out.extend_from_slice(&[0u8; 36]);

    // bV5GammaRed, bV5GammaGreen, bV5GammaBlue
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());

    // bV5Intent: LCS_GM_IMAGES = 4
    out.extend_from_slice(&4u32.to_le_bytes());

    // bV5ProfileData, bV5ProfileSize, bV5Reserved
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
}

/// Row stride in bytes (rows padded to 4-byte boundary).
fn row_stride(width: u32, bpp: u16) -> u32 {
    let row_bytes = width * u32::from(bpp) / 8;
    row_bytes.next_multiple_of(4)
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Convert a PNG byte stream into a `CF_DIBV5` byte payload.
///
/// The output is a `BITMAPV5HEADER` (124 bytes) followed by pixel data (no
/// `BITMAPFILEHEADER`). Only colour types 2 (RGB) and 6 (RGBA) at bit depth 8
/// are supported.
pub(crate) fn png_to_dib(png: &[u8]) -> Result<Vec<u8>, ClipboardError> {
    let bad = |msg: &'static str| ClipboardError::io_other(msg);

    // --- Verify PNG signature ---
    if png.len() < 8 || png[..8] != PNG_SIGNATURE {
        return Err(bad("not a PNG file"));
    }

    // --- Parse chunks ---
    let mut pos = 8;
    let mut width: u32 = 0;
    let mut height: u32 = 0;
    let mut bit_depth: u8 = 0;
    let mut color_type: u8 = 0;
    let mut got_ihdr = false;
    let mut idat_data: Vec<u8> = Vec::new();

    loop {
        let (chunk_type, chunk_data, next_pos) = read_chunk(png, pos)?;
        pos = next_pos;

        match &chunk_type {
            b"IHDR" => {
                if chunk_data.len() < 13 {
                    return Err(bad("truncated IHDR"));
                }
                width = u32::from_be_bytes(chunk_data[0..4].try_into().unwrap());
                height = u32::from_be_bytes(chunk_data[4..8].try_into().unwrap());
                bit_depth = chunk_data[8];
                color_type = chunk_data[9];
                // compression(10), filter(11), interlace(12) — we don't use them.
                got_ihdr = true;
            }
            b"IDAT" => {
                idat_data.extend_from_slice(chunk_data);
            }
            b"IEND" => break,
            _ => {} // skip ancillary chunks
        }
    }

    if !got_ihdr {
        return Err(bad("PNG missing IHDR chunk"));
    }
    if idat_data.is_empty() {
        return Err(bad("PNG missing IDAT chunk"));
    }

    // We only handle 8-bit RGB and RGBA.
    let channels: usize = match (color_type, bit_depth) {
        (PNG_COLOR_RGB, 8) => 3,
        (PNG_COLOR_RGBA, 8) => 4,
        _ => return Err(bad("unsupported PNG format")),
    };

    if width == 0 || height == 0 {
        return Err(bad("PNG has zero dimension"));
    }

    // --- Inflate IDAT (zlib-wrapped deflate) ---
    let raw = miniz_oxide::inflate::decompress_to_vec_zlib(&idat_data)
        .map_err(|_| bad("PNG IDAT deflate error"))?;

    let row_bytes = width as usize * channels;
    let expected = height as usize * (1 + row_bytes); // 1 filter byte per row
    if raw.len() != expected {
        return Err(bad("PNG IDAT decompressed size mismatch"));
    }

    // --- Unfilter scanlines → top-down RGBA/RGB rows ---
    let mut rows: Vec<Vec<u8>> = Vec::with_capacity(height as usize);
    let zero_row = vec![0u8; row_bytes];
    for r in 0..height as usize {
        let src = &raw[r * (1 + row_bytes)..];
        let filter = src[0];
        let mut row = src[1..1 + row_bytes].to_vec();
        let prev: &[u8] = rows.last().map(Vec::as_slice).unwrap_or(&zero_row);
        unfilter_row(filter, &mut row, prev, channels)?;
        rows.push(row);
    }

    // --- Build DIB ---
    let bpp: u16 = (channels * 8) as u16;
    let stride = row_stride(width, bpp) as usize;
    let image_size = stride * height as usize;

    let mut out = Vec::with_capacity(DIB_HEADER_SIZE as usize + image_size);
    write_dib_header(&mut out, width, height, bpp);

    debug_assert_eq!(
        out.len(),
        DIB_HEADER_SIZE as usize,
        "DIB header must be 124 bytes"
    );

    // DIB rows are bottom-up: write rows in reverse order.
    // Convert channel order: PNG RGBA -> DIB BGRA, PNG RGB -> DIB BGR.
    for r in (0..height as usize).rev() {
        let row = &rows[r];
        let mut dib_row = Vec::with_capacity(stride);
        if channels == 4 {
            // RGBA -> BGRA
            for px in row.chunks_exact(4) {
                dib_row.push(px[2]); // B
                dib_row.push(px[1]); // G
                dib_row.push(px[0]); // R
                dib_row.push(px[3]); // A
            }
        } else {
            // RGB -> BGR
            for px in row.chunks_exact(3) {
                dib_row.push(px[2]); // B
                dib_row.push(px[1]); // G
                dib_row.push(px[0]); // R
            }
        }
        // Pad row to 4-byte boundary.
        dib_row.resize(stride, 0);
        out.extend_from_slice(&dib_row);
    }

    Ok(out)
}

/// Convert a `CF_DIBV5` byte payload into a PNG byte stream.
///
/// The input must begin with a `BITMAPV5HEADER` (124 bytes) — no
/// `BITMAPFILEHEADER`. Both bottom-up (positive height) and top-down (negative
/// height) DIBs are accepted. Only 24 bpp and 32 bpp are supported.
pub(crate) fn dib_to_png(dib: &[u8]) -> Result<Vec<u8>, ClipboardError> {
    let bad = |msg: &'static str| ClipboardError::io_other(msg);

    if dib.len() < DIB_HEADER_SIZE as usize {
        return Err(bad("DIB too short for BITMAPV5HEADER"));
    }

    // Parse header fields (all little-endian).
    let size = u32::from_le_bytes(dib[0..4].try_into().unwrap());
    if size != DIB_HEADER_SIZE {
        return Err(bad("DIB header size is not 124 (not a BITMAPV5HEADER)"));
    }

    let width = i32::from_le_bytes(dib[4..8].try_into().unwrap());
    let height_raw = i32::from_le_bytes(dib[8..12].try_into().unwrap());
    let bpp = u16::from_le_bytes(dib[14..16].try_into().unwrap());

    if width <= 0 {
        return Err(bad("DIB width must be positive"));
    }

    let (height, top_down) = if height_raw < 0 {
        ((-height_raw) as u32, true)
    } else if height_raw > 0 {
        (height_raw as u32, false)
    } else {
        return Err(bad("DIB height is zero"));
    };

    let width = width as u32;

    let channels: usize = match bpp {
        32 => 4,
        24 => 3,
        _ => return Err(bad("unsupported DIB bit depth (only 24 and 32 bpp)")),
    };

    let stride = row_stride(width, bpp) as usize;
    let image_data = &dib[DIB_HEADER_SIZE as usize..];
    let expected = stride * height as usize;

    if image_data.len() < expected {
        return Err(bad("DIB pixel data shorter than expected"));
    }

    // --- Extract rows, converting BGR(A) -> RGB(A) ---
    let mut rows: Vec<Vec<u8>> = Vec::with_capacity(height as usize);
    for r in 0..height as usize {
        let src = &image_data[r * stride..r * stride + width as usize * channels];
        let mut row = Vec::with_capacity(width as usize * channels);
        if channels == 4 {
            // BGRA -> RGBA
            for px in src.chunks_exact(4) {
                row.push(px[2]); // R
                row.push(px[1]); // G
                row.push(px[0]); // B
                row.push(px[3]); // A
            }
        } else {
            // BGR -> RGB
            for px in src.chunks_exact(3) {
                row.push(px[2]); // R
                row.push(px[1]); // G
                row.push(px[0]); // B
            }
        }
        rows.push(row);
    }

    // Bottom-up DIB stores rows in reverse order — flip so index 0 is the top.
    if !top_down {
        rows.reverse();
    }

    // --- Build PNG ---
    let color_type: u8 = if channels == 4 {
        PNG_COLOR_RGBA
    } else {
        PNG_COLOR_RGB
    };
    let row_bytes = width as usize * channels;

    // IHDR data
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.push(8); // bit depth
    ihdr.push(color_type);
    ihdr.push(0); // compression method
    ihdr.push(0); // filter method
    ihdr.push(0); // interlace method

    // Build raw filtered scanline data (filter type 0 = None per row).
    let mut raw = Vec::with_capacity(height as usize * (1 + row_bytes));
    for row in &rows {
        raw.push(0); // filter type: None
        raw.extend_from_slice(row);
    }

    // Compress with zlib.
    let compressed = miniz_oxide::deflate::compress_to_vec_zlib(
        &raw,
        miniz_oxide::deflate::CompressionLevel::DefaultLevel as u8,
    );

    let mut out = Vec::new();
    out.extend_from_slice(&PNG_SIGNATURE);
    write_chunk(&mut out, b"IHDR", &ihdr);
    write_chunk(&mut out, b"IDAT", &compressed);
    write_chunk(&mut out, b"IEND", &[]);

    Ok(out)
}

// ---------------------------------------------------------------------------
// Tests — compiled on every host (Linux, macOS, Windows).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // CRC32 sanity check
    // -----------------------------------------------------------------------

    #[test]
    fn crc32_known_value() {
        // CRC32 of the ASCII string "123456789" must be 0xCBF43926.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    // -----------------------------------------------------------------------
    // Synthetic PNG builder (for tests only)
    // -----------------------------------------------------------------------

    /// Build a minimal valid PNG from raw RGBA or RGB pixels (top-down, no filtering).
    fn build_png(width: u32, height: u32, channels: usize, pixels: &[u8]) -> Vec<u8> {
        assert_eq!(pixels.len(), width as usize * height as usize * channels);
        let color_type: u8 = if channels == 4 {
            PNG_COLOR_RGBA
        } else {
            PNG_COLOR_RGB
        };
        let row_bytes = width as usize * channels;

        let mut ihdr = Vec::with_capacity(13);
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.push(8);
        ihdr.push(color_type);
        ihdr.push(0);
        ihdr.push(0);
        ihdr.push(0);

        // Raw scanlines with filter type 0.
        let mut raw = Vec::with_capacity(height as usize * (1 + row_bytes));
        for r in 0..height as usize {
            raw.push(0); // filter: None
            raw.extend_from_slice(&pixels[r * row_bytes..(r + 1) * row_bytes]);
        }

        let compressed = miniz_oxide::deflate::compress_to_vec_zlib(
            &raw,
            miniz_oxide::deflate::CompressionLevel::DefaultLevel as u8,
        );

        let mut out = Vec::new();
        out.extend_from_slice(&PNG_SIGNATURE);
        write_chunk(&mut out, b"IHDR", &ihdr);
        write_chunk(&mut out, b"IDAT", &compressed);
        write_chunk(&mut out, b"IEND", &[]);
        out
    }

    /// Extract raw RGBA/RGB pixels from a PNG produced by `dib_to_png`.
    /// Returns (width, height, channels, pixel_bytes).
    fn decode_png(png: &[u8]) -> (u32, u32, usize, Vec<u8>) {
        assert_eq!(&png[..8], &PNG_SIGNATURE, "bad signature");
        let mut pos = 8;
        let mut width = 0u32;
        let mut height = 0u32;
        let mut color_type = 0u8;
        let mut idat: Vec<u8> = Vec::new();

        loop {
            let (chunk_type, chunk_data, next) = read_chunk(png, pos).unwrap();
            pos = next;
            match &chunk_type {
                b"IHDR" => {
                    width = u32::from_be_bytes(chunk_data[0..4].try_into().unwrap());
                    height = u32::from_be_bytes(chunk_data[4..8].try_into().unwrap());
                    color_type = chunk_data[9];
                }
                b"IDAT" => idat.extend_from_slice(chunk_data),
                b"IEND" => break,
                _ => {}
            }
        }

        let channels: usize = match color_type {
            PNG_COLOR_RGBA => 4,
            PNG_COLOR_RGB => 3,
            _ => panic!("unexpected color_type {color_type}"),
        };
        let row_bytes = width as usize * channels;
        let raw = miniz_oxide::inflate::decompress_to_vec_zlib(&idat).unwrap();
        let mut pixels = Vec::with_capacity(width as usize * height as usize * channels);
        let zero = vec![0u8; row_bytes];
        let mut rows: Vec<Vec<u8>> = Vec::new();
        for r in 0..height as usize {
            let filter = raw[r * (1 + row_bytes)];
            let mut row =
                raw[r * (1 + row_bytes) + 1..r * (1 + row_bytes) + 1 + row_bytes].to_vec();
            let prev = rows.last().map(Vec::as_slice).unwrap_or(&zero);
            unfilter_row(filter, &mut row, prev, channels).unwrap();
            rows.push(row);
        }
        for row in rows {
            pixels.extend_from_slice(&row);
        }
        (width, height, channels, pixels)
    }

    // -----------------------------------------------------------------------
    // Round-trip tests
    // -----------------------------------------------------------------------

    #[test]
    fn rgba_2x2_round_trip() {
        // 2x2 RGBA image with distinct pixel colours.
        #[rustfmt::skip]
        let pixels: Vec<u8> = vec![
            255,   0,   0, 255, // top-left:  opaque red
              0, 255,   0, 128, // top-right: semi-green
              0,   0, 255, 255, // bot-left:  opaque blue
            128, 128, 128, 200, // bot-right: semi-grey
        ];
        let png = build_png(2, 2, 4, &pixels);
        let dib = png_to_dib(&png).expect("png_to_dib failed");
        let png2 = dib_to_png(&dib).expect("dib_to_png failed");
        let (w, h, ch, recovered) = decode_png(&png2);
        assert_eq!((w, h, ch), (2, 2, 4));
        assert_eq!(recovered, pixels, "RGBA 2x2 round-trip pixel mismatch");
    }

    #[test]
    fn rgb_2x2_round_trip() {
        // 2x2 RGB image.
        #[rustfmt::skip]
        let pixels: Vec<u8> = vec![
            255,   0,   0, // top-left:  red
              0, 255,   0, // top-right: green
              0,   0, 255, // bot-left:  blue
            128, 128, 128, // bot-right: grey
        ];
        let png = build_png(2, 2, 3, &pixels);
        let dib = png_to_dib(&png).expect("png_to_dib failed");
        let png2 = dib_to_png(&dib).expect("dib_to_png failed");
        let (w, h, ch, recovered) = decode_png(&png2);
        assert_eq!((w, h, ch), (2, 2, 3));
        assert_eq!(recovered, pixels, "RGB 2x2 round-trip pixel mismatch");
    }

    #[test]
    fn single_row_rgba() {
        // 4x1 RGBA — edge case: minimal IDAT with a single row.
        let pixels: Vec<u8> = (0..16).collect(); // 4 pixels x 4 channels
        let png = build_png(4, 1, 4, &pixels);
        let dib = png_to_dib(&png).expect("png_to_dib failed");
        let png2 = dib_to_png(&dib).expect("dib_to_png failed");
        let (w, h, _, recovered) = decode_png(&png2);
        assert_eq!((w, h), (4, 1));
        assert_eq!(recovered, pixels);
    }

    #[test]
    fn rgb_row_padding_3x2() {
        // 3x2 RGB: row is 9 bytes, padded to 12 in DIB (4-byte alignment).
        #[rustfmt::skip]
        let pixels: Vec<u8> = vec![
            10, 20, 30,   // row 0 px 0
            40, 50, 60,   // row 0 px 1
            70, 80, 90,   // row 0 px 2
            11, 21, 31,   // row 1 px 0
            41, 51, 61,   // row 1 px 1
            71, 81, 91,   // row 1 px 2
        ];
        let png = build_png(3, 2, 3, &pixels);
        let dib = png_to_dib(&png).expect("png_to_dib failed");

        // Verify the stride in the DIB pixel block is 12 bytes.
        let stride = row_stride(3, 24) as usize;
        assert_eq!(stride, 12, "3x24bpp stride must be 12");

        // Pixel data starts at byte 124 (DIB_HEADER_SIZE).
        let pixel_block = &dib[DIB_HEADER_SIZE as usize..];
        assert_eq!(
            pixel_block.len(),
            stride * 2,
            "pixel block must be stride * height bytes"
        );

        let png2 = dib_to_png(&dib).expect("dib_to_png failed");
        let (w, h, ch, recovered) = decode_png(&png2);
        assert_eq!((w, h, ch), (3, 2, 3));
        assert_eq!(recovered, pixels, "3x2 RGB padding round-trip mismatch");
    }

    #[test]
    fn top_down_dib_parses_correctly() {
        // Build a DIB with negative height (top-down) and verify that
        // dib_to_png produces the same image as a bottom-up DIB of the same pixels.
        #[rustfmt::skip]
        let pixels: Vec<u8> = vec![
            255,   0,   0, 255, // row 0
              0, 255,   0, 255, // row 1
        ];
        let png = build_png(1, 2, 4, &pixels);
        let dib_bu = png_to_dib(&png).expect("png_to_dib failed"); // bottom-up

        // Manually create a top-down DIB from the bottom-up one.
        // Flip the height sign in the header and reverse the pixel rows.
        let mut dib_td = dib_bu.clone();
        // height is at bytes 8..12 (i32 LE). Negate it.
        let h = i32::from_le_bytes(dib_td[8..12].try_into().unwrap());
        let neg = (-h).to_le_bytes();
        dib_td[8..12].copy_from_slice(&neg);
        // Reverse the pixel rows (each row is stride bytes wide).
        let stride = row_stride(1, 32) as usize; // 4 bytes for 1 RGBA pixel
        let pixel_data = &mut dib_td[DIB_HEADER_SIZE as usize..];
        let n_rows = 2;
        for i in 0..n_rows / 2 {
            let j = n_rows - 1 - i;
            let (lo, hi) = (i * stride, j * stride);
            // Swap rows i and j.
            for k in 0..stride {
                pixel_data.swap(lo + k, hi + k);
            }
        }

        let png_bu = dib_to_png(&dib_bu).expect("bottom-up dib_to_png failed");
        let png_td = dib_to_png(&dib_td).expect("top-down dib_to_png failed");

        let (_, _, _, pixels_bu) = decode_png(&png_bu);
        let (_, _, _, pixels_td) = decode_png(&png_td);
        assert_eq!(
            pixels_bu, pixels_td,
            "top-down and bottom-up DIBs must decode to same image"
        );
        assert_eq!(pixels_bu, pixels, "decoded pixels must match original");
    }

    #[test]
    fn bad_png_signature_is_error() {
        let mut bad = vec![0u8; 64];
        bad[0] = 0x00; // break the signature
        assert!(
            png_to_dib(&bad).is_err(),
            "expected error for bad PNG signature"
        );
    }

    #[test]
    fn dib_header_size_mismatch_is_error() {
        // Build a 124-byte header but with bV5Size != 124.
        let mut dib = vec![0u8; 200];
        // Write size = 40 (BITMAPINFOHEADER) instead of 124.
        dib[0..4].copy_from_slice(&40u32.to_le_bytes());
        assert!(
            dib_to_png(&dib).is_err(),
            "expected error when DIB header size != 124"
        );
    }

    #[test]
    fn unsupported_png_palette_is_error() {
        // Build a PNG with color_type = 3 (indexed/palette) — unsupported.
        let mut fake_ihdr = vec![0u8; 13];
        fake_ihdr[0..4].copy_from_slice(&1u32.to_be_bytes()); // width = 1
        fake_ihdr[4..8].copy_from_slice(&1u32.to_be_bytes()); // height = 1
        fake_ihdr[8] = 8; // bit depth
        fake_ihdr[9] = 3; // color_type: indexed (palette)
        // compression, filter, interlace already 0.

        let mut png = Vec::new();
        png.extend_from_slice(&PNG_SIGNATURE);
        write_chunk(&mut png, b"IHDR", &fake_ihdr);
        // Minimal (bogus) IDAT and IEND so the parser reaches the format check.
        let dummy_idat = miniz_oxide::deflate::compress_to_vec_zlib(
            &[0u8, 0u8], // filter byte + one pixel
            miniz_oxide::deflate::CompressionLevel::DefaultLevel as u8,
        );
        write_chunk(&mut png, b"IDAT", &dummy_idat);
        write_chunk(&mut png, b"IEND", &[]);

        assert!(png_to_dib(&png).is_err(), "expected error for palette PNG");
    }

    #[test]
    fn unsupported_png_16bit_is_error() {
        // Build a PNG with bit_depth = 16 (unsupported).
        let mut fake_ihdr = vec![0u8; 13];
        fake_ihdr[0..4].copy_from_slice(&1u32.to_be_bytes());
        fake_ihdr[4..8].copy_from_slice(&1u32.to_be_bytes());
        fake_ihdr[8] = 16; // bit depth: 16-bit
        fake_ihdr[9] = PNG_COLOR_RGB;

        let mut png = Vec::new();
        png.extend_from_slice(&PNG_SIGNATURE);
        write_chunk(&mut png, b"IHDR", &fake_ihdr);
        let dummy_idat = miniz_oxide::deflate::compress_to_vec_zlib(
            &[0u8, 0u8, 0u8, 0u8], // filter + 1 RGB 16-bit pixel (3x2 bytes) — intentionally wrong
            miniz_oxide::deflate::CompressionLevel::DefaultLevel as u8,
        );
        write_chunk(&mut png, b"IDAT", &dummy_idat);
        write_chunk(&mut png, b"IEND", &[]);

        assert!(png_to_dib(&png).is_err(), "expected error for 16-bit PNG");
    }

    #[test]
    fn dib_header_size_field_verified() {
        // Verify that write_dib_header emits exactly 124 bytes.
        let mut buf = Vec::new();
        write_dib_header(&mut buf, 4, 4, 32);
        assert_eq!(buf.len(), 124, "DIB header must be exactly 124 bytes");
    }
}
