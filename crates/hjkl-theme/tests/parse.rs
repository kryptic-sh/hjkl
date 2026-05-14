use hjkl_theme::{Color, Modifiers, StyleSpec, Theme, ThemeError};

// ---------------------------------------------------------------------------
// 1. Minimal theme
// ---------------------------------------------------------------------------

#[test]
fn minimal_theme_parses() {
    let toml = include_str!("fixtures/minimal.toml");
    let theme = Theme::from_toml_str(toml).unwrap();

    let kw = theme.captures.get("@keyword").unwrap();
    assert_eq!(kw.fg, Some(Color::rgb(0xcb, 0xa6, 0xf7)));
    assert_eq!(kw.bg, None);

    let st = theme.captures.get("@string").unwrap();
    assert_eq!(st.fg, Some(Color::rgb(0xa6, 0xe3, 0xa1)));

    // UI should be all-default
    assert!(theme.ui.background.is_none());
    assert!(theme.ui.foreground.is_none());
    assert!(theme.ui.statusline.is_none());
}

// ---------------------------------------------------------------------------
// 2. Palette interning
// ---------------------------------------------------------------------------

#[test]
fn palette_ref_resolves() {
    // In TOML, root key-values must come BEFORE [section] headers.
    let toml = r##"
"@function" = "$blue"

[palette]
blue = "#89b4fa"
"##;
    let theme = Theme::from_toml_str(toml).unwrap();
    let spec = theme.captures.get("@function").unwrap();
    assert_eq!(spec.fg, Some(Color::rgb(0x89, 0xb4, 0xfa)));
}

// ---------------------------------------------------------------------------
// 3. Shorthand vs full StyleSpec
// ---------------------------------------------------------------------------

#[test]
fn shorthand_and_full_match() {
    let toml = r##"
"@a" = "#abc123"
"@b" = { fg = "#abc123" }
"##;
    let theme = Theme::from_toml_str(toml).unwrap();
    let a = theme.captures.get("@a").unwrap();
    let b = theme.captures.get("@b").unwrap();
    assert_eq!(a.fg, b.fg);
    assert_eq!(a.bg, b.bg);
    assert_eq!(a.modifiers, b.modifiers);
}

// ---------------------------------------------------------------------------
// 4. Modifiers
// ---------------------------------------------------------------------------

#[test]
fn modifiers_parse() {
    let toml = r##"
"@keyword" = { fg = "#cba6f7", modifiers = ["bold", "italic"] }
"##;
    let theme = Theme::from_toml_str(toml).unwrap();
    let m = theme.captures.get("@keyword").unwrap().modifiers;
    assert!(m.bold);
    assert!(m.italic);
    assert!(!m.underline);
    assert!(!m.reverse);
    assert!(!m.strikethrough);
}

#[test]
fn unknown_modifier_errors() {
    let toml = r##"
"@x" = { fg = "#abc123", modifiers = ["glow"] }
"##;
    let err = Theme::from_toml_str(toml).unwrap_err();
    assert!(matches!(err, ThemeError::BadModifier(s) if s == "glow"));
}

// ---------------------------------------------------------------------------
// 5. Fallback chain
// ---------------------------------------------------------------------------

#[test]
fn resolve_falls_back() {
    let toml = r##"
"@function" = "#89b4fa"
"##;
    let theme = Theme::from_toml_str(toml).unwrap();

    // Exact miss, fallback hit
    assert!(theme.captures.get("@function.builtin").is_none());
    let spec = theme.captures.resolve("@function.builtin").unwrap();
    assert_eq!(spec.fg, Some(Color::rgb(0x89, 0xb4, 0xfa)));

    // Direct hit
    let direct = theme.captures.resolve("@function").unwrap();
    assert_eq!(direct.fg, Some(Color::rgb(0x89, 0xb4, 0xfa)));

    // Total miss
    assert!(theme.captures.resolve("@nonexistent").is_none());
}

#[test]
fn resolve_multi_segment_fallback() {
    let toml = r##"
"@function" = "#89b4fa"
"##;
    let theme = Theme::from_toml_str(toml).unwrap();
    // Three levels deep: @function.builtin.constructor -> @function.builtin -> @function
    let spec = theme
        .captures
        .resolve("@function.builtin.constructor")
        .unwrap();
    assert_eq!(spec.fg, Some(Color::rgb(0x89, 0xb4, 0xfa)));
}

// ---------------------------------------------------------------------------
// 6. Hex parsing
// ---------------------------------------------------------------------------

#[test]
fn hex_shorthand_parses() {
    assert_eq!(
        Color::from_hex_str("#abc").unwrap(),
        Color::rgb(0xaa, 0xbb, 0xcc)
    );
    assert_eq!(
        Color::from_hex_str("#aabbcc").unwrap(),
        Color::rgb(0xaa, 0xbb, 0xcc)
    );
    assert_eq!(
        Color::from_hex_str("#aabbccdd").unwrap(),
        Color::rgba(0xaa, 0xbb, 0xcc, 0xdd)
    );
}

#[test]
fn hex_bad_inputs_error() {
    assert!(matches!(
        Color::from_hex_str("abc"),
        Err(ThemeError::BadHex(_))
    ));
    assert!(matches!(
        Color::from_hex_str("#zzz"),
        Err(ThemeError::BadHex(_))
    ));
    assert!(matches!(
        Color::from_hex_str("#12"),
        Err(ThemeError::BadHex(_))
    ));
}

// ---------------------------------------------------------------------------
// 7. Bad palette ref
// ---------------------------------------------------------------------------

#[test]
fn bad_palette_ref_errors() {
    let toml = include_str!("fixtures/bad_palette_ref.toml");
    let err = Theme::from_toml_str(toml).unwrap_err();
    assert!(
        matches!(err, ThemeError::UnresolvedPalette(ref n) if n == "undefined"),
        "expected UnresolvedPalette(\"undefined\"), got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// 8. UI dotted keys
// ---------------------------------------------------------------------------

#[test]
fn ui_dotted_keys_deserialize() {
    let toml = r##"
[ui]
background            = "#1e1e2e"
foreground            = "#cdd6f4"
"statusline.inactive" = { fg = "#6c7086", bg = "#181825" }
"gutter.current"      = "#cdd6f4"
"##;
    let theme = Theme::from_toml_str(toml).unwrap();
    assert_eq!(theme.ui.background, Some(Color::rgb(0x1e, 0x1e, 0x2e)));
    assert_eq!(theme.ui.foreground, Some(Color::rgb(0xcd, 0xd6, 0xf4)));

    let si = theme.ui.statusline_inactive.unwrap();
    assert_eq!(si.fg, Some(Color::rgb(0x6c, 0x70, 0x86)));
    assert_eq!(si.bg, Some(Color::rgb(0x18, 0x18, 0x25)));

    assert_eq!(theme.ui.gutter_current, Some(Color::rgb(0xcd, 0xd6, 0xf4)));
    assert!(theme.ui.statusline.is_none());
}

// ---------------------------------------------------------------------------
// 9. Catppuccin subset fixture round-trips cleanly
// ---------------------------------------------------------------------------

#[test]
fn catppuccin_subset_parses() {
    let toml = include_str!("fixtures/catppuccin_subset.toml");
    let theme = Theme::from_toml_str(toml).unwrap();

    // Palette interned
    assert_eq!(theme.palette["blue"], Color::rgb(0x89, 0xb4, 0xfa));

    // 8 capture keys present
    for key in [
        "@keyword",
        "@function",
        "@function.builtin",
        "@string",
        "@comment",
        "@variable",
        "@number",
        "@type",
    ] {
        assert!(theme.captures.get(key).is_some(), "missing capture {key}");
    }

    // UI surface fields
    assert_eq!(theme.ui.background, Some(Color::rgb(0x1e, 0x1e, 0x2e)));
    assert!(theme.ui.statusline.is_some());
    assert!(theme.ui.statusline_inactive.is_some());
    assert_eq!(
        theme.ui.diagnostic_error,
        Some(Color::rgb(0xf3, 0x8b, 0xa8))
    );
    assert_eq!(theme.ui.diagnostic_warn, Some(Color::rgb(0xf9, 0xe2, 0xaf)));

    // Modifiers on @keyword
    let kw = theme.captures.get("@keyword").unwrap();
    assert!(kw.modifiers.bold);
    assert!(!kw.modifiers.italic);

    // @function shorthand resolves to blue
    let fun = theme.captures.get("@function").unwrap();
    assert_eq!(fun.fg, Some(Color::rgb(0x89, 0xb4, 0xfa)));

    // Fallback: @function.builtin.constructor -> @function.builtin
    let resolved = theme
        .captures
        .resolve("@function.builtin.constructor")
        .unwrap();
    assert!(resolved.modifiers.italic);

    // StyleSpec equality: shorthand "=" vs full "{ fg = ... }"
    let expected = StyleSpec {
        fg: Some(Color::rgb(0x89, 0xb4, 0xfa)),
        bg: None,
        modifiers: Modifiers::default(),
    };
    assert_eq!(*fun, expected);
}
