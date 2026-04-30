//! Dynamic library loaders for libxcb and libwayland-client.
//!
//! Symbols are stored in `OnceLock` structs so the load happens at most once
//! per process. Missing libraries return [`ClipboardError::LibNotFound`].
