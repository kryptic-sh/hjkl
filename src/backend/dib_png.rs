//! Windows DIB ↔ PNG conversion via `miniz_oxide`.
//!
//! Modern apps use the `"PNG"` registered format directly; legacy apps use
//! `CF_DIBV5`. This module converts between the two so both directions work.
