//! Detect the active backend and whether OSC 52 is in use.
//!
//! Run with: `cargo run --example backend_detect`
//!
//! Useful for diagnosing silent OSC 52 fallback when no display is reachable.

use hjkl_clipboard::{Clipboard, ClipboardError};

fn main() -> Result<(), ClipboardError> {
    match Clipboard::new() {
        Ok(cb) => {
            let name = cb.backend_name();
            println!("backend: {name}");
            if name == "osc52" {
                println!("note: OSC 52 is write-only; get/available return UnsupportedMime");
            }
        }
        Err(e) => {
            println!("clipboard unavailable: {e}");
        }
    }
    Ok(())
}
