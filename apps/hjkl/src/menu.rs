//! Context menu binding shim.
//!
//! Re-exports the agnostic model from `hjkl-menu` and the ratatui renderer
//! from `hjkl-menu-tui`. All menu logic lives in those crates; this module is
//! the glue that makes `crate::menu::*` available to the rest of `apps/hjkl`.

// MenuItem is part of the public surface used by tests and downstream callers.
#[allow(unused_imports)]
pub use hjkl_menu::{
    ContextMenu, MenuAction, MenuActionKind, MenuItem, build_code_menu, build_gutter_menu,
    build_picker_menu, build_split_border_menu, build_status_line_menu, build_tab_menu,
};
pub use hjkl_menu_tui::{MenuTheme, bounding_rect, render};
