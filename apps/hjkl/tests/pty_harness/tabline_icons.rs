//! e2e: tabline filetype icons are painted with their per-filetype devicon
//! color (#260), not the tab's monochrome foreground.
//!
//! Opens two tabs of different filetypes under a real pty, then reads the
//! tabline row (row 0) and asserts each icon glyph cell carries the exact RGB
//! devicon color from `hjkl_icons::file_color_for_path`. Before the fix the icon
//! inherited the tab fg (monochrome), so the per-filetype colors would not
//! appear — and the two icons would share one color.

use super::harness::TerminalSession;
use hjkl_icons::IconMode;
use std::path::PathBuf;

fn seed(suffix: &str) -> (tempfile::NamedTempFile, PathBuf) {
    let f = tempfile::Builder::new()
        .suffix(suffix)
        .tempfile()
        .expect("create temp file");
    f.as_file().sync_all().ok();
    let path = f.path().to_owned();
    (f, path)
}

#[test]
fn tabline_icons_use_per_filetype_devicon_color() {
    // Two filetypes with known, distinct devicon colors in hjkl-icons:
    //   rust → (0xde, 0xa5, 0x84), markdown → (0x51, 0x9a, 0xba).
    let (_rs_keep, rs) = seed(".rs");
    let (_md_keep, md) = seed(".md");

    let mut s = TerminalSession::spawn_with_file(&rs);
    // Open the second file in a NEW tab so the tabline (>1 tab) is shown.
    s.keys(&format!(":tabe {}<Enter>", md.display()));

    // Test env resolves icon mode to Nerd (no config + xterm-256color).
    let rs_glyph = hjkl_icons::file_icon_for_path(&rs, IconMode::Nerd).to_string();
    let md_glyph = hjkl_icons::file_icon_for_path(&md, IconMode::Nerd).to_string();
    let rs_color = hjkl_icons::file_color_for_path(&rs).expect("rust has a devicon color");
    let md_color = hjkl_icons::file_color_for_path(&md).expect("markdown has a devicon color");
    // Sanity: the two filetypes must differ so the assertion is meaningful.
    assert_ne!(rs_color, md_color);

    // Tabline is row 0 when shown. Poll for the icons to render.
    let rs_fg = s
        .wait_cell_fg_of_symbol(0, &rs_glyph, 2000)
        .expect("rust icon glyph never rendered on the tabline");
    let md_fg = s
        .cell_fg_of_symbol(0, &md_glyph)
        .expect("markdown icon glyph not on the tabline");

    assert_eq!(
        rs_fg,
        vt100::Color::Rgb(rs_color.0, rs_color.1, rs_color.2),
        "rust tab icon must use the rust devicon color"
    );
    assert_eq!(
        md_fg,
        vt100::Color::Rgb(md_color.0, md_color.1, md_color.2),
        "markdown tab icon must use the markdown devicon color"
    );
    // And the two icons are colored differently (the pre-fix monochrome bug
    // painted both with the same tab fg).
    assert_ne!(rs_fg, md_fg, "per-filetype icons must not share one color");
}
