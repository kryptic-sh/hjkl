//! Fuzzy picker subsystem for hjkl-based apps.
//!
//! Provides the non-generic `Picker` harness driven by a `Box<dyn PickerLogic>`,
//! built-in file (`FileSource`) and grep (`RgSource`) sources, a fuzzy scorer,
//! and preview infrastructure. The `PickerLogic` trait allows apps to add custom
//! sources without modifying picker internals.

pub mod logic;
pub mod picker;
pub mod preview;
pub mod score;
pub mod source;

// Flat re-exports for ergonomic use by consumers.
pub use logic::{PickerAction, PickerEvent, PickerLogic, RequeryMode};
pub use picker::Picker;
pub use preview::{
    PREVIEW_MAX_BYTES, PREVIEW_MAX_LINES, PreviewSpans, build_preview_spans, load_preview,
};
pub use score::score;
pub use source::{
    FileSource, GrepBackend, RgMatch, RgSource, detect_grep_backend, extract_json_string,
    extract_json_u32, parse_grep_line, parse_rg_json_line,
};
