//! Thin re-export of `hjkl_xdg` for backward-compat. The XDG resolver
//! moved to `hjkl-xdg` in 0.6.x; consumers that imported
//! `hjkl_bonsai::runtime::xdg::data_home` continue to work.
pub use hjkl_xdg::{cache_home, data_home};
