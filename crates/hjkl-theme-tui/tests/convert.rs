use hjkl_theme::{Color, Modifiers, StyleSpec, Theme};
use hjkl_theme_tui::ToRatatui;
use ratatui::style::{Color as RColor, Modifier as RMod, Style as RStyle};

// 1. Color conversion — alpha dropped silently.
#[test]
fn color_rgb_drops_alpha() {
    let c = Color::rgb(0x12, 0x34, 0x56);
    assert_eq!(c.to_ratatui(), RColor::Rgb(0x12, 0x34, 0x56));
}

#[test]
fn color_rgba_drops_alpha() {
    let c = Color::rgba(0xAA, 0xBB, 0xCC, 0x80);
    assert_eq!(c.to_ratatui(), RColor::Rgb(0xAA, 0xBB, 0xCC));
}

// 2. Modifier combinations.
#[test]
fn modifiers_bold_italic() {
    let m = Modifiers {
        bold: true,
        italic: true,
        ..Default::default()
    };
    assert_eq!(m.to_ratatui(), RMod::BOLD | RMod::ITALIC);
}

#[test]
fn modifiers_empty() {
    let m = Modifiers::default();
    assert_eq!(m.to_ratatui(), RMod::empty());
}

// 3. StyleSpec fg set, bg absent.
#[test]
fn stylespec_fg_only() {
    let red = Color::rgb(0xFF, 0x00, 0x00);
    let spec = StyleSpec {
        fg: Some(red),
        bg: None,
        modifiers: Modifiers {
            bold: true,
            ..Default::default()
        },
    };
    let got = spec.to_ratatui();
    let want = RStyle::default()
        .fg(RColor::Rgb(0xFF, 0x00, 0x00))
        .add_modifier(RMod::BOLD);
    assert_eq!(got, want);
    // bg must not be set.
    assert_eq!(got.bg, None);
}

// 4. All five modifier flags map to the right ratatui constant.
#[test]
fn modifier_bold() {
    let m = Modifiers {
        bold: true,
        ..Default::default()
    };
    assert!(m.to_ratatui().contains(RMod::BOLD));
}

#[test]
fn modifier_italic() {
    let m = Modifiers {
        italic: true,
        ..Default::default()
    };
    assert!(m.to_ratatui().contains(RMod::ITALIC));
}

#[test]
fn modifier_underline() {
    let m = Modifiers {
        underline: true,
        ..Default::default()
    };
    assert!(m.to_ratatui().contains(RMod::UNDERLINED));
}

#[test]
fn modifier_reverse() {
    let m = Modifiers {
        reverse: true,
        ..Default::default()
    };
    assert!(m.to_ratatui().contains(RMod::REVERSED));
}

#[test]
fn modifier_strikethrough() {
    let m = Modifiers {
        strikethrough: true,
        ..Default::default()
    };
    assert!(m.to_ratatui().contains(RMod::CROSSED_OUT));
}

// 5. Round-trip via Theme::from_toml_str + capture lookup.
#[test]
fn theme_roundtrip_function_capture() {
    // Root key-values must precede [section] headers in TOML.
    let toml_str = concat!(
        "\"@function\" = \"$blue\"\n",
        "\n",
        "[palette]\n",
        "blue = \"#89b4fa\"\n",
    );
    let theme = Theme::from_toml_str(toml_str).expect("parse failed");
    let spec = theme
        .captures
        .resolve("@function")
        .expect("@function not found");
    let style = spec.to_ratatui();
    assert_eq!(style.fg, Some(RColor::Rgb(0x89, 0xb4, 0xfa)));
    assert_eq!(style.bg, None);
}
