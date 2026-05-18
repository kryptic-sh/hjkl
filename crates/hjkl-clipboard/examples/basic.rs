//! Sync clipboard: set, get, and clear.
//!
//! Run with: `cargo run --example basic`
//!
//! Requires a display server (Wayland, X11) or falls back to OSC 52 (SSH).

use hjkl_clipboard::{Clipboard, ClipboardError, MimeType, Selection};

fn main() -> Result<(), ClipboardError> {
    let cb = Clipboard::new()?;

    // Write plain text.
    cb.set(Selection::Clipboard, MimeType::Text, b"hello from hjkl")?;

    // Read it back.
    let bytes = cb.get(Selection::Clipboard, MimeType::Text)?;
    println!("got: {}", String::from_utf8_lossy(&bytes));

    // Clear.
    cb.clear(Selection::Clipboard)?;
    println!("cleared");

    Ok(())
}
