//! Windows `CF_HTML` clipboard format header wrap/unwrap.
//!
//! Windows requires a specific ASCII header prepended to HTML payloads. This
//! module encodes and decodes that header for round-trip correctness.
