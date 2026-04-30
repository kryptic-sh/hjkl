//! Singleton background thread for Linux backends (X11 + Wayland).
//!
//! Spawned lazily on first clipboard operation, lives until process exit.
//! Accepts `Request` messages and dispatches to the active backend.
//! The bg thread keeps selections alive independently of `Clipboard` handle
//! lifetimes — drop of last handle does NOT kill the thread.
