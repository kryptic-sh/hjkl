//! Custom MIME types and URI list helpers.
//!
//! Run with: `cargo run --example custom_mime`

use hjkl_clipboard::{Clipboard, ClipboardError, MimeType, Selection, Uri};
use std::path::PathBuf;

fn main() -> Result<(), ClipboardError> {
    let cb = Clipboard::new()?;

    // Raw passthrough — no translation applied.
    cb.set(
        Selection::Clipboard,
        MimeType::Custom("application/x-my-format".into()),
        b"\x00\x01\x02",
    )?;

    let data = cb.get(
        Selection::Clipboard,
        MimeType::Custom("application/x-my-format".into()),
    )?;
    println!("custom: {} bytes", data.len());

    // URI list.
    let uris = vec![
        Uri::File(PathBuf::from("/home/user/document.pdf")),
        Uri::Other("https://example.com".into()),
    ];
    cb.set_uri_list(Selection::Clipboard, &uris)?;

    let back = cb.get_uri_list(Selection::Clipboard)?;
    println!("uris: {back:?}");

    Ok(())
}
