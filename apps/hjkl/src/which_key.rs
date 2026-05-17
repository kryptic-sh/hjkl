//! Which-key popup helpers — thin shim re-exporting `hjkl_which_key`.
//!
//! All logic lives in the `hjkl-which-key` crate. This module is kept for
//! backward-compat so existing `crate::which_key::*` call sites compile unchanged.
pub use hjkl_which_key::*;
