//! Async clipboard: set and get using pollster (zero-dep executor).
//!
//! Run with: `cargo run --example async_basic`
//!
//! Uses `pollster` as a lightweight block_on executor — no tokio needed.
//! Add `pollster = "0.3"` to `[dev-dependencies]` to run this example.

use hjkl_clipboard::{Clipboard, ClipboardError, MimeType, Selection};

fn main() -> Result<(), ClipboardError> {
    pollster::block_on(run())
}

async fn run() -> Result<(), ClipboardError> {
    let cb = Clipboard::new()?;

    cb.set_async(Selection::Clipboard, MimeType::Text, b"async write")
        .await?;

    let text = cb.get_async(Selection::Clipboard, MimeType::Text).await?;
    println!("got: {}", String::from_utf8_lossy(&text));

    let mimes = cb.available_async(Selection::Clipboard).await?;
    println!("mimes: {mimes:?}");

    cb.clear_async(Selection::Clipboard).await?;
    println!("cleared");

    Ok(())
}
