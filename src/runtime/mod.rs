//! Runtime grammar loader (Phase 2.x).
//!
//! Reads the `bonsai.toml` manifest and resolves a path or language name to a
//! [`LangSpec`]. Source acquisition, compilation, and dynamic loading land in
//! later phases; this module currently only handles the lookup tables.
//!
//! The default registry is built from the embedded `bonsai.toml` shipped with
//! the crate via [`GrammarRegistry::embedded`].

mod manifest;
mod registry;
mod source;

pub use manifest::{LangSpec, Manifest};
pub use registry::GrammarRegistry;
pub use source::SourceCache;
