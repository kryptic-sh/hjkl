//! Parser + AST for a CSS subset used to drive declarative UI styling.
//!
//! Toolkit-agnostic — produces a `Stylesheet` of `Rule`s plus a `resolve()`
//! step that yields the property bag for a single node. Pair with an
//! adapter crate (e.g. `hjkl-css-floem`) to map onto a specific UI
//! framework's style builder.
//!
//! Supported:
//! - Type selectors (`label`, `row`), class selectors (`.prompt`),
//!   pseudo-class selectors (`:hover`, `:focus`, `:active`, `:disabled`,
//!   `:selected`), and combinations of the three on the same simple
//!   selector.
//! - Compound selectors with descendant (` `), child (`>`),
//!   adjacent-sibling (`+`), and general-sibling (`~`) combinators.
//! - Properties: `color`, `background-color`, `padding`, `margin`,
//!   `width`, `height`, `display`, `flex-direction`, `flex-grow`,
//!   `flex-shrink`, `flex-basis`, `align-items`, `justify-content`,
//!   `gap`, `row-gap`, `column-gap`, `border`, `border-{top,right,bottom,left}`,
//!   `border-width`, `border-color`, `border-radius`, `outline`,
//!   `font-family`, `font-size`, `font-weight`, `font-style`,
//!   `text-align`, `line-height`.
//! - Values: hex / `rgb()` / `rgba()` / named colors (CSS Level 1 + extras),
//!   lengths in `px` / `%` / unitless (treated as px), keywords, `auto`,
//!   unitless numbers, font-family lists, border shorthands.

pub mod ast;
pub mod error;
pub mod parse;
pub mod resolve;
pub mod value;

pub use ast::{
    Combinator, Declaration, Node, PseudoClass, Rule, Selector, SimpleSelector, Stylesheet,
};
pub use error::ParseError;
pub use parse::parse;
pub use resolve::ResolvedStyle;
pub use value::{Color, Length, SideValue, Value, expand_side_set, expand_sides};

#[cfg(test)]
mod tests {
    use super::*;

    fn s(css: &str) -> Stylesheet {
        parse(css).unwrap()
    }

    fn n<'a>(element: &'a str, classes: &'a [&'a str]) -> Node<'a> {
        Node { element, classes }
    }

    #[test]
    fn parses_type_selector_and_one_color_prop() {
        let sheet = s("label { color: #fff; }");
        assert_eq!(sheet.rules.len(), 1);
        assert_eq!(
            sheet.rules[0].selectors[0].parts[0].element.as_deref(),
            Some("label")
        );
        let resolved = sheet.resolve(&n("label", &[]), &[], &[], None);
        assert_eq!(
            resolved.get("color"),
            Some(&Value::Color(Color::rgb(0xff, 0xff, 0xff)))
        );
    }

    #[test]
    fn class_selector_filters() {
        let sheet = s(".prompt { color: #f00; }");
        let hit = sheet.resolve(&n("label", &["prompt"]), &[], &[], None);
        let miss = sheet.resolve(&n("label", &[]), &[], &[], None);
        assert!(hit.get("color").is_some());
        assert!(miss.is_empty());
    }

    #[test]
    fn pseudo_class_only_applies_in_state() {
        let sheet = s(".row { color: #aaa; } .row:hover { color: #fff; }");
        let base = sheet.resolve(&n("row", &["row"]), &[], &[], None);
        let hover = sheet.resolve(&n("row", &["row"]), &[], &[], Some(PseudoClass::Hover));
        assert_eq!(
            base.get("color"),
            Some(&Value::Color(Color::rgb(0xaa, 0xaa, 0xaa)))
        );
        assert_eq!(
            hover.get("color"),
            Some(&Value::Color(Color::rgb(0xff, 0xff, 0xff)))
        );
    }

    #[test]
    fn padding_shorthand_one_value() {
        let sheet = s("button { padding: 10px; }");
        let resolved = sheet.resolve(&n("button", &[]), &[], &[], None);
        let Value::LengthSet(set) = resolved.get("padding").unwrap() else {
            panic!("expected LengthSet");
        };
        assert_eq!(set, &vec![Length::Px(10.0)]);
        let expanded = expand_sides(set).unwrap();
        assert_eq!(expanded, [Length::Px(10.0); 4]);
    }

    #[test]
    fn padding_shorthand_two_values_top_right() {
        let sheet = s("button { padding: 10px 20px; }");
        let r = sheet.resolve(&n("button", &[]), &[], &[], None);
        let Value::LengthSet(set) = r.get("padding").unwrap() else {
            unreachable!()
        };
        let exp = expand_sides(set).unwrap();
        assert_eq!(
            exp,
            [
                Length::Px(10.0),
                Length::Px(20.0),
                Length::Px(10.0),
                Length::Px(20.0)
            ]
        );
    }

    #[test]
    fn cascade_specificity_class_beats_type() {
        let sheet = s("label { color: #aaa; } .head { color: #fff; }");
        let r = sheet.resolve(&n("label", &["head"]), &[], &[], None);
        assert_eq!(
            r.get("color"),
            Some(&Value::Color(Color::rgb(0xff, 0xff, 0xff)))
        );
    }

    #[test]
    fn cascade_source_order_breaks_ties() {
        let sheet = s(".a { color: #001; } .a { color: #002; }");
        let r = sheet.resolve(&n("x", &["a"]), &[], &[], None);
        assert_eq!(r.get("color"), Some(&Value::Color(Color::rgb(0, 0, 0x22))));
    }

    #[test]
    fn rgb_and_rgba_functions() {
        let sheet = s("x { color: rgb(255, 128, 0); background-color: rgba(0, 0, 0, 0.5); }");
        let r = sheet.resolve(&n("x", &[]), &[], &[], None);
        assert_eq!(
            r.get("color"),
            Some(&Value::Color(Color::rgb(0xff, 0x80, 0)))
        );
        assert_eq!(
            r.get("background-color"),
            Some(&Value::Color(Color::rgba(0, 0, 0, 128)))
        );
    }

    #[test]
    fn selector_list_applies_to_all() {
        let sheet = s(".a, .b { color: #fff; }");
        assert!(
            sheet
                .resolve(&n("x", &["a"]), &[], &[], None)
                .get("color")
                .is_some()
        );
        assert!(
            sheet
                .resolve(&n("x", &["b"]), &[], &[], None)
                .get("color")
                .is_some()
        );
        assert!(sheet.resolve(&n("x", &["c"]), &[], &[], None).is_empty());
    }

    #[test]
    fn unitless_number_parses_as_px() {
        let sheet = s("x { width: 100; }");
        let r = sheet.resolve(&n("x", &[]), &[], &[], None);
        assert_eq!(r.get("width"), Some(&Value::Length(Length::Px(100.0))));
    }

    #[test]
    fn percent_length() {
        let sheet = s("x { width: 50%; }");
        let r = sheet.resolve(&n("x", &[]), &[], &[], None);
        assert_eq!(r.get("width"), Some(&Value::Length(Length::Percent(50.0))));
    }

    #[test]
    fn keyword_value() {
        let sheet = s("x { display: flex; }");
        let r = sheet.resolve(&n("x", &[]), &[], &[], None);
        assert_eq!(r.get("display"), Some(&Value::Keyword("flex".to_string())));
    }

    #[test]
    fn hex_short_form_expands() {
        let sheet = s("x { color: #abc; }");
        let r = sheet.resolve(&n("x", &[]), &[], &[], None);
        assert_eq!(
            r.get("color"),
            Some(&Value::Color(Color::rgb(0xaa, 0xbb, 0xcc)))
        );
    }

    #[test]
    fn unknown_pseudo_class_dropped() {
        // `:nonsense` makes the whole rule malformed → cssparser drops
        // the rule, the stylesheet ends up empty. Lenient parsing per
        // CSS spec; previously this returned `Err` from `parse()`.
        let sheet = parse(":nonsense { color: #fff; }").unwrap();
        assert!(sheet.rules.is_empty());
    }

    #[test]
    fn descendant_combinator_parses() {
        // `.a .b { … }` must now parse into one rule with a Descendant combinator.
        let sheet = parse(".a .b { color: #fff; }").unwrap();
        assert_eq!(sheet.rules.len(), 1);
        let sel = &sheet.rules[0].selectors[0];
        assert_eq!(sel.combinators, vec![Combinator::Descendant]);
        assert_eq!(sel.parts.len(), 2);
    }

    #[test]
    fn descendant_combinator_through_comment() {
        // `.a /* x */ .b` — cssparser emits two whitespace tokens around
        // the comment. The compound-selector parser must collapse them
        // before deciding whether what follows is a combinator.
        for css in [
            ".a /* x */ .b { color: #fff; }",
            ".a   /* x */   .b { color: #fff; }",
            ".a/* x */ .b { color: #fff; }",
        ] {
            let sheet = parse(css).unwrap();
            assert_eq!(sheet.rules.len(), 1, "input: {css}");
            let sel = &sheet.rules[0].selectors[0];
            assert_eq!(
                sel.combinators,
                vec![Combinator::Descendant],
                "input: {css}"
            );
            assert_eq!(sel.parts.len(), 2, "input: {css}");
        }
    }

    #[test]
    fn pseudo_class_is_case_insensitive() {
        let sheet = s(".row:HOVER { color: #fff; } .row:Focus { color: #aaa; }");
        let h = sheet.resolve(&n("row", &["row"]), &[], &[], Some(PseudoClass::Hover));
        let f = sheet.resolve(&n("row", &["row"]), &[], &[], Some(PseudoClass::Focus));
        assert_eq!(
            h.get("color"),
            Some(&Value::Color(Color::rgb(0xff, 0xff, 0xff)))
        );
        assert_eq!(
            f.get("color"),
            Some(&Value::Color(Color::rgb(0xaa, 0xaa, 0xaa)))
        );
    }

    #[test]
    fn bad_declaration_does_not_drop_neighbours() {
        // `font: 12px Arial` has no PropertyKind so it routes through the
        // Unknown branch — but `12px Arial` has two tokens and fails
        // expect_exhausted. The CSS spec says a malformed declaration must
        // be skipped, leaving siblings intact.
        let sheet = s("x { font: 12px Arial; color: #fff; padding: 4px; }");
        let r = sheet.resolve(&n("x", &[]), &[], &[], None);
        assert_eq!(
            r.get("color"),
            Some(&Value::Color(Color::rgb(0xff, 0xff, 0xff)))
        );
        let Value::LengthSet(set) = r.get("padding").unwrap() else {
            unreachable!()
        };
        assert_eq!(set, &vec![Length::Px(4.0)]);
        assert!(r.get("font").is_none(), "font must not have leaked through");
    }

    #[test]
    fn important_flag_is_tolerated() {
        // Smoke test that a single `!important` declaration resolves
        // cleanly; the cascade behaviour against competing rules lives in
        // `important_beats_higher_specificity` and
        // `important_loses_to_later_important` below.
        let sheet = s("x { color: #fff !important; }");
        let r = sheet.resolve(&n("x", &[]), &[], &[], None);
        assert_eq!(
            r.get("color"),
            Some(&Value::Color(Color::rgb(0xff, 0xff, 0xff)))
        );
    }

    #[test]
    fn at_rules_are_silently_skipped() {
        // `@charset` (statement at-rule) and `@media` (block at-rule)
        // must not abort the surrounding stylesheet.
        let sheet = s(r#"
            @charset "utf-8";
            @media (min-width: 100px) { .ignored { color: #000; } }
            .visible { color: #fff; }
        "#);
        let v = sheet.resolve(&n("x", &["visible"]), &[], &[], None);
        assert_eq!(
            v.get("color"),
            Some(&Value::Color(Color::rgb(0xff, 0xff, 0xff)))
        );
        let i = sheet.resolve(&n("x", &["ignored"]), &[], &[], None);
        assert!(i.is_empty(), "@media block contents must not leak");
    }

    #[test]
    fn descendant_combinator_all_shapes_parse() {
        // All shapes that were previously dropped now parse into one rule each
        // with the correct combinator.
        for css in [
            "label span { color: #fff; }",
            "label .b { color: #fff; }",
            "label :hover { color: #fff; }",
            ".a label { color: #fff; }",
            ":hover label { color: #fff; }",
        ] {
            let sheet = parse(css).unwrap();
            assert_eq!(
                sheet.rules.len(),
                1,
                "descendant combinator must parse to one rule: {css}"
            );
            assert_eq!(
                sheet.rules[0].selectors[0].combinators,
                vec![Combinator::Descendant],
                "expected Descendant combinator: {css}"
            );
        }
    }

    #[test]
    fn important_flag_surfaces_on_declaration() {
        let sheet = parse(".a { color: #fff !important; padding: 4px; }").unwrap();
        let decls = &sheet.rules[0].declarations;
        let color = decls.iter().find(|d| d.property == "color").unwrap();
        let padding = decls.iter().find(|d| d.property == "padding").unwrap();
        assert!(color.important, "!important must survive on the AST");
        assert!(!padding.important);
    }

    #[test]
    fn important_beats_higher_specificity() {
        // `.important !important` must override `.specific:hover` which
        // has higher specificity (20 vs 10) — important wins regardless.
        let sheet = s(".important { color: #fff !important; } \
                       .specific:hover { color: #000; }");
        let r = sheet.resolve(
            &n("x", &["important", "specific"]),
            &[],
            &[],
            Some(PseudoClass::Hover),
        );
        assert_eq!(
            r.get("color"),
            Some(&Value::Color(Color::rgb(0xff, 0xff, 0xff)))
        );
    }

    #[test]
    fn important_loses_to_later_important() {
        // Within the !important group, source order still applies — the
        // later !important wins on equal specificity.
        let sheet = s(".a { color: #001 !important; } .a { color: #002 !important; }");
        let r = sheet.resolve(&n("x", &["a"]), &[], &[], None);
        assert_eq!(r.get("color"), Some(&Value::Color(Color::rgb(0, 0, 0x22))));
    }

    #[test]
    fn malformed_rule_does_not_drop_neighbours() {
        // The `:nonsense` selector is invalid → cssparser drops that whole
        // rule, but the second rule must still land in the stylesheet.
        let sheet = s(":nonsense { color: #000; } \
                       .good { color: #fff; }");
        let r = sheet.resolve(&n("x", &["good"]), &[], &[], None);
        assert_eq!(
            r.get("color"),
            Some(&Value::Color(Color::rgb(0xff, 0xff, 0xff)))
        );
    }

    #[test]
    fn unknown_color_name_rejected_for_color_property() {
        // Previously this would silently parse as Value::Keyword and leak
        // through. Property-aware value parsing rejects it as a bad
        // declaration, which the cascade then skips.
        let sheet = s("x { color: nonsense; }");
        let r = sheet.resolve(&n("x", &[]), &[], &[], None);
        assert!(r.get("color").is_none());
    }

    // ---- Phase 2 tests -------------------------------------------------------

    // Layout

    #[test]
    fn display_flex() {
        let r = s("x { display: flex; }").resolve(&n("x", &[]), &[], &[], None);
        assert_eq!(r.get("display"), Some(&Value::Keyword("flex".into())));
    }

    #[test]
    fn display_unknown_rejected() {
        let r = s("x { display: inline; }").resolve(&n("x", &[]), &[], &[], None);
        assert!(r.get("display").is_none());
    }

    #[test]
    fn flex_direction() {
        let r = s("x { flex-direction: column; }").resolve(&n("x", &[]), &[], &[], None);
        assert_eq!(
            r.get("flex-direction"),
            Some(&Value::Keyword("column".into()))
        );
    }

    #[test]
    fn flex_grow_and_shrink() {
        let r = s("x { flex-grow: 2; flex-shrink: 0; }").resolve(&n("x", &[]), &[], &[], None);
        assert_eq!(r.get("flex-grow"), Some(&Value::Number(2.0)));
        assert_eq!(r.get("flex-shrink"), Some(&Value::Number(0.0)));
    }

    #[test]
    fn flex_grow_negative_dropped() {
        // CSS spec: flex-grow / flex-shrink must be >= 0. The bad
        // declaration is dropped per the standard cascade rules.
        let r = s("x { flex-grow: -1; }").resolve(&n("x", &[]), &[], &[], None);
        assert!(r.get("flex-grow").is_none());
    }

    #[test]
    fn flex_basis_length() {
        let r = s("x { flex-basis: 200px; }").resolve(&n("x", &[]), &[], &[], None);
        assert_eq!(r.get("flex-basis"), Some(&Value::Length(Length::Px(200.0))));
    }

    #[test]
    fn flex_basis_auto() {
        let r = s("x { flex-basis: auto; }").resolve(&n("x", &[]), &[], &[], None);
        assert_eq!(r.get("flex-basis"), Some(&Value::Auto));
    }

    #[test]
    fn align_items() {
        let r = s("x { align-items: center; }").resolve(&n("x", &[]), &[], &[], None);
        assert_eq!(r.get("align-items"), Some(&Value::Keyword("center".into())));
    }

    #[test]
    fn justify_content() {
        let r = s("x { justify-content: space-between; }").resolve(&n("x", &[]), &[], &[], None);
        assert_eq!(
            r.get("justify-content"),
            Some(&Value::Keyword("space-between".into()))
        );
    }

    #[test]
    fn gap() {
        let r = s("x { gap: 8px; row-gap: 4px; column-gap: 2px; }").resolve(
            &n("x", &[]),
            &[],
            &[],
            None,
        );
        assert_eq!(r.get("gap"), Some(&Value::Length(Length::Px(8.0))));
        assert_eq!(r.get("row-gap"), Some(&Value::Length(Length::Px(4.0))));
        assert_eq!(r.get("column-gap"), Some(&Value::Length(Length::Px(2.0))));
    }

    // Box — border

    #[test]
    fn border_shorthand() {
        let r = s("x { border: 1px solid #fff; }").resolve(&n("x", &[]), &[], &[], None);
        assert_eq!(
            r.get("border"),
            Some(&Value::Border {
                width: Length::Px(1.0),
                color: Color::rgb(0xff, 0xff, 0xff),
            })
        );
    }

    #[test]
    fn border_out_of_order_tokens() {
        let r = s("x { border: solid 1px #fff; }").resolve(&n("x", &[]), &[], &[], None);
        assert_eq!(
            r.get("border"),
            Some(&Value::Border {
                width: Length::Px(1.0),
                color: Color::rgb(0xff, 0xff, 0xff),
            })
        );
    }

    #[test]
    fn border_none_is_transparent_zero() {
        // The most common CSS reset; the round-2 review caught this being
        // rejected. `border: none` must resolve to a structurally present
        // but visually invisible border.
        let r = s("x { border: none; }").resolve(&n("x", &[]), &[], &[], None);
        assert_eq!(
            r.get("border"),
            Some(&Value::Border {
                width: Length::Px(0.0),
                color: Color::rgba(0, 0, 0, 0),
            })
        );
    }

    #[test]
    fn border_no_color_rejected() {
        // `border: 1px solid` — missing color → declaration dropped.
        let r = s("x { border: 1px solid; }").resolve(&n("x", &[]), &[], &[], None);
        assert!(r.get("border").is_none());
    }

    #[test]
    fn border_unknown_style_keyword_ignored() {
        // `dashed`/`dotted`/`double` etc. parse without dropping the
        // declaration — floem has no border-style model, so the style
        // token is accepted and ignored, same as `solid`.
        for css in [
            "x { border: 2px dashed #f00; }",
            "x { border: 2px dotted #f00; }",
            "x { border: 2px double #f00; }",
            "x { border: 2px groove #f00; }",
        ] {
            let r = s(css).resolve(&n("x", &[]), &[], &[], None);
            assert_eq!(
                r.get("border"),
                Some(&Value::Border {
                    width: Length::Px(2.0),
                    color: Color::rgb(0xff, 0x00, 0x00),
                }),
                "input: {css}"
            );
        }
    }

    #[test]
    fn border_side() {
        let r = s("x { border-top: 2px solid red; }").resolve(&n("x", &[]), &[], &[], None);
        assert_eq!(
            r.get("border-top"),
            Some(&Value::Border {
                width: Length::Px(2.0),
                color: Color::rgb(0xff, 0x00, 0x00),
            })
        );
    }

    #[test]
    fn border_width_and_color() {
        let r =
            s("x { border-width: 3px; border-color: blue; }").resolve(&n("x", &[]), &[], &[], None);
        assert_eq!(
            r.get("border-width"),
            Some(&Value::LengthSet(vec![Length::Px(3.0)]))
        );
        assert_eq!(
            r.get("border-color"),
            Some(&Value::Color(Color::rgb(0x00, 0x00, 0xff)))
        );
    }

    #[test]
    fn border_width_four_side_shorthand() {
        let r = s("x { border-width: 1px 2px 3px 4px; }").resolve(&n("x", &[]), &[], &[], None);
        let Value::LengthSet(set) = r.get("border-width").unwrap() else {
            panic!("expected LengthSet");
        };
        assert_eq!(
            set,
            &vec![
                Length::Px(1.0),
                Length::Px(2.0),
                Length::Px(3.0),
                Length::Px(4.0),
            ]
        );
    }

    #[test]
    fn border_radius() {
        let r = s("x { border-radius: 4px 8px; }").resolve(&n("x", &[]), &[], &[], None);
        let Value::LengthSet(set) = r.get("border-radius").unwrap() else {
            panic!("expected LengthSet");
        };
        assert_eq!(set, &vec![Length::Px(4.0), Length::Px(8.0)]);
    }

    #[test]
    fn outline_shorthand() {
        let r = s("x { outline: 1px solid #000; }").resolve(&n("x", &[]), &[], &[], None);
        assert_eq!(
            r.get("outline"),
            Some(&Value::Border {
                width: Length::Px(1.0),
                color: Color::rgb(0x00, 0x00, 0x00),
            })
        );
    }

    // Sizing — auto

    #[test]
    fn width_auto() {
        let r = s("x { width: auto; }").resolve(&n("x", &[]), &[], &[], None);
        assert_eq!(r.get("width"), Some(&Value::Auto));
    }

    #[test]
    fn height_auto() {
        let r = s("x { height: auto; }").resolve(&n("x", &[]), &[], &[], None);
        assert_eq!(r.get("height"), Some(&Value::Auto));
    }

    #[test]
    fn margin_auto() {
        let r = s("x { margin: auto; }").resolve(&n("x", &[]), &[], &[], None);
        assert_eq!(r.get("margin"), Some(&Value::Auto));
    }

    #[test]
    fn margin_mixed_auto() {
        // `margin: 4px auto` — mixed → SideSet
        let r = s("x { margin: 4px auto; }").resolve(&n("x", &[]), &[], &[], None);
        let Value::SideSet(sides) = r.get("margin").unwrap() else {
            panic!("expected SideSet");
        };
        assert_eq!(sides[0], SideValue::Length(Length::Px(4.0)));
        assert_eq!(sides[1], SideValue::Auto);
    }

    #[test]
    fn margin_all_lengths_downcasts_to_length_set() {
        // All-length margin → LengthSet (backward compat with adapters).
        let r = s("x { margin: 4px 8px; }").resolve(&n("x", &[]), &[], &[], None);
        assert!(
            matches!(r.get("margin"), Some(Value::LengthSet(_))),
            "expected LengthSet"
        );
    }

    #[test]
    fn expand_side_set_mirrors_css_shorthand() {
        let one = vec![SideValue::Auto];
        let two = vec![SideValue::Length(Length::Px(4.0)), SideValue::Auto];
        let three = vec![
            SideValue::Length(Length::Px(1.0)),
            SideValue::Auto,
            SideValue::Length(Length::Px(3.0)),
        ];
        let four = vec![
            SideValue::Length(Length::Px(1.0)),
            SideValue::Length(Length::Px(2.0)),
            SideValue::Length(Length::Px(3.0)),
            SideValue::Length(Length::Px(4.0)),
        ];
        assert_eq!(expand_side_set(&one).unwrap(), [SideValue::Auto; 4]);
        let exp_two = expand_side_set(&two).unwrap();
        assert_eq!(exp_two[0], SideValue::Length(Length::Px(4.0)));
        assert_eq!(exp_two[1], SideValue::Auto);
        assert_eq!(exp_two[2], SideValue::Length(Length::Px(4.0)));
        assert_eq!(exp_two[3], SideValue::Auto);
        let exp_three = expand_side_set(&three).unwrap();
        assert_eq!(
            exp_three[1], exp_three[3],
            "right == left when 3 values given"
        );
        let exp_four = expand_side_set(&four).unwrap();
        assert_eq!(exp_four[3], SideValue::Length(Length::Px(4.0)));
        // Out-of-range returns None.
        assert!(expand_side_set(&[]).is_none());
        assert!(expand_side_set(&[SideValue::Auto; 5]).is_none());
    }

    // Typography

    #[test]
    fn font_family_quoted_and_keyword() {
        let r = s(r#"x { font-family: "Hack Nerd Font", monospace; }"#).resolve(
            &n("x", &[]),
            &[],
            &[],
            None,
        );
        let Value::FontFamilyList(list) = r.get("font-family").unwrap() else {
            panic!("expected FontFamilyList");
        };
        assert_eq!(
            list,
            &vec!["Hack Nerd Font".to_string(), "monospace".to_string()]
        );
    }

    #[test]
    fn font_family_single_ident() {
        let r = s("x { font-family: monospace; }").resolve(&n("x", &[]), &[], &[], None);
        let Value::FontFamilyList(list) = r.get("font-family").unwrap() else {
            panic!("expected FontFamilyList");
        };
        assert_eq!(list, &vec!["monospace".to_string()]);
    }

    #[test]
    fn font_family_trailing_comma_dropped() {
        // `font-family: "Hack",` is malformed CSS; the bad declaration is
        // dropped and the stylesheet survives.
        let r =
            s(r#"x { font-family: "Hack",; color: #fff; }"#).resolve(&n("x", &[]), &[], &[], None);
        assert!(r.get("font-family").is_none());
        assert_eq!(
            r.get("color"),
            Some(&Value::Color(Color::rgb(0xff, 0xff, 0xff)))
        );
    }

    #[test]
    fn font_size() {
        let r = s("x { font-size: 16px; }").resolve(&n("x", &[]), &[], &[], None);
        assert_eq!(r.get("font-size"), Some(&Value::Length(Length::Px(16.0))));
    }

    #[test]
    fn font_weight_numeric() {
        let r = s("x { font-weight: 350; }").resolve(&n("x", &[]), &[], &[], None);
        assert_eq!(r.get("font-weight"), Some(&Value::Number(350.0)));
    }

    #[test]
    fn font_weight_bold_keyword() {
        let r = s("x { font-weight: bold; }").resolve(&n("x", &[]), &[], &[], None);
        assert_eq!(r.get("font-weight"), Some(&Value::Keyword("bold".into())));
    }

    #[test]
    fn font_weight_bolder_rejected() {
        let r = s("x { font-weight: bolder; }").resolve(&n("x", &[]), &[], &[], None);
        assert!(r.get("font-weight").is_none());
    }

    #[test]
    fn font_style() {
        let r = s("x { font-style: italic; }").resolve(&n("x", &[]), &[], &[], None);
        assert_eq!(r.get("font-style"), Some(&Value::Keyword("italic".into())));
    }

    #[test]
    fn text_align() {
        let r = s("x { text-align: center; }").resolve(&n("x", &[]), &[], &[], None);
        assert_eq!(r.get("text-align"), Some(&Value::Keyword("center".into())));
    }

    #[test]
    fn line_height_unitless() {
        let r = s("x { line-height: 1.5; }").resolve(&n("x", &[]), &[], &[], None);
        assert_eq!(r.get("line-height"), Some(&Value::Number(1.5)));
    }

    #[test]
    fn line_height_px() {
        let r = s("x { line-height: 24px; }").resolve(&n("x", &[]), &[], &[], None);
        assert_eq!(r.get("line-height"), Some(&Value::Length(Length::Px(24.0))));
    }

    // Issue #3 — font-style: oblique

    #[test]
    fn font_style_oblique_accepted() {
        let r = s("x { font-style: oblique; }").resolve(&n("x", &[]), &[], &[], None);
        assert_eq!(r.get("font-style"), Some(&Value::Keyword("oblique".into())));
    }

    #[test]
    fn font_style_unknown_keyword_dropped() {
        // `weird` is not in the allowed list → declaration dropped, rule survives.
        let r = s("x { font-style: weird; color: #fff; }").resolve(&n("x", &[]), &[], &[], None);
        assert!(r.get("font-style").is_none());
        assert_eq!(
            r.get("color"),
            Some(&Value::Color(Color::rgb(0xff, 0xff, 0xff)))
        );
    }

    // Issue #4 — per-side border-{side}-color longhands

    #[test]
    fn border_top_color_resolves() {
        for side in ["top", "right", "bottom", "left"] {
            let css = format!("x {{ border-{side}-color: red; }}");
            let r = s(&css).resolve(&n("x", &[]), &[], &[], None);
            let prop = format!("border-{side}-color");
            assert_eq!(
                r.get(&prop),
                Some(&Value::Color(Color::rgb(0xff, 0x00, 0x00))),
                "border-{side}-color must resolve as Color"
            );
        }
    }

    // Issue #5 — font-weight range clamping

    #[test]
    fn font_weight_out_of_range_dropped() {
        // Values outside 1..=1000 or with fractional parts are invalid.
        for bad in ["9999", "-100", "0.5", "0"] {
            let css = format!("x {{ font-weight: {bad}; color: #fff; }}");
            let r = s(&css).resolve(&n("x", &[]), &[], &[], None);
            assert!(
                r.get("font-weight").is_none(),
                "font-weight: {bad} should be dropped"
            );
            // sibling declaration must survive
            assert!(
                r.get("color").is_some(),
                "color must survive bad font-weight: {bad}"
            );
        }
        // In-range integer must still pass.
        let r = s("x { font-weight: 700; }").resolve(&n("x", &[]), &[], &[], None);
        assert_eq!(r.get("font-weight"), Some(&Value::Number(700.0)));
        // Keyword forms must still pass.
        for kw in ["bold", "normal"] {
            let css = format!("x {{ font-weight: {kw}; }}");
            let r = s(&css).resolve(&n("x", &[]), &[], &[], None);
            assert_eq!(
                r.get("font-weight"),
                Some(&Value::Keyword(kw.into())),
                "font-weight: {kw} keyword must resolve"
            );
        }
        // Unknown keywords (e.g. `bolder`, `lighter`) are rejected.
        let r = s("x { font-weight: bolder; color: #fff; }").resolve(&n("x", &[]), &[], &[], None);
        assert!(
            r.get("font-weight").is_none(),
            "unsupported font-weight keyword must be dropped"
        );
        // Boundary integers — both ends of the CSS spec range.
        for ok in ["1", "1000"] {
            let css = format!("x {{ font-weight: {ok}; }}");
            let r = s(&css).resolve(&n("x", &[]), &[], &[], None);
            assert_eq!(
                r.get("font-weight"),
                Some(&Value::Number(ok.parse().unwrap())),
                "font-weight: {ok} boundary value must resolve"
            );
        }
    }

    // Named color expansion

    #[test]
    fn named_colors_level1() {
        let cases: &[(&str, Color)] = &[
            ("silver", Color::rgb(0xc0, 0xc0, 0xc0)),
            ("maroon", Color::rgb(0x80, 0x00, 0x00)),
            ("purple", Color::rgb(0x80, 0x00, 0x80)),
            ("fuchsia", Color::rgb(0xff, 0x00, 0xff)),
            ("lime", Color::rgb(0x00, 0xff, 0x00)),
            ("olive", Color::rgb(0x80, 0x80, 0x00)),
            ("yellow", Color::rgb(0xff, 0xff, 0x00)),
            ("navy", Color::rgb(0x00, 0x00, 0x80)),
            ("teal", Color::rgb(0x00, 0x80, 0x80)),
            ("aqua", Color::rgb(0x00, 0xff, 0xff)),
        ];
        for (name, expected) in cases {
            let css = format!("x {{ color: {name}; }}");
            let r = s(&css).resolve(&n("x", &[]), &[], &[], None);
            assert_eq!(
                r.get("color"),
                Some(&Value::Color(*expected)),
                "named color `{name}` mismatch"
            );
        }
    }

    #[test]
    fn named_colors_extras() {
        let cases: &[(&str, Color)] = &[
            ("gray", Color::rgb(0x80, 0x80, 0x80)),
            ("grey", Color::rgb(0x80, 0x80, 0x80)),
            ("cyan", Color::rgb(0x00, 0xff, 0xff)),
            ("magenta", Color::rgb(0xff, 0x00, 0xff)),
            ("orange", Color::rgb(0xff, 0xa5, 0x00)),
            ("brown", Color::rgb(0xa5, 0x2a, 0x2a)),
            ("pink", Color::rgb(0xff, 0xc0, 0xcb)),
        ];
        for (name, expected) in cases {
            let css = format!("x {{ color: {name}; }}");
            let r = s(&css).resolve(&n("x", &[]), &[], &[], None);
            assert_eq!(
                r.get("color"),
                Some(&Value::Color(*expected)),
                "named color `{name}` mismatch"
            );
        }
    }

    // ---- Combinator tests ----------------------------------------------------

    #[test]
    fn descendant_no_match_without_ancestor() {
        // `.outer .target { color: #fff; }` — target has no `.outer` ancestor.
        let sheet = s(".outer .target { color: #fff; }");
        let r = sheet.resolve(&n("div", &["target"]), &[], &[], None);
        assert!(r.get("color").is_none());
    }

    #[test]
    fn descendant_match_with_ancestor() {
        let sheet = s(".outer .target { color: #fff; }");
        let ancestors = [n("div", &["outer"])];
        let r = sheet.resolve(&n("div", &["target"]), &ancestors, &[], None);
        assert_eq!(
            r.get("color"),
            Some(&Value::Color(Color::rgb(0xff, 0xff, 0xff)))
        );
    }

    #[test]
    fn descendant_match_with_distant_ancestor() {
        // `.outer` is a grandparent — Descendant must still match.
        let sheet = s(".outer .target { color: #fff; }");
        let ancestors = [n("root", &[]), n("div", &["outer"]), n("div", &["mid"])];
        let r = sheet.resolve(&n("span", &["target"]), &ancestors, &[], None);
        assert_eq!(
            r.get("color"),
            Some(&Value::Color(Color::rgb(0xff, 0xff, 0xff)))
        );
    }

    #[test]
    fn child_match_immediate_parent() {
        let sheet = s(".outer > .target { color: #fff; }");
        let ancestors = [n("div", &["outer"])];
        let r = sheet.resolve(&n("span", &["target"]), &ancestors, &[], None);
        assert_eq!(
            r.get("color"),
            Some(&Value::Color(Color::rgb(0xff, 0xff, 0xff)))
        );
    }

    #[test]
    fn child_no_match_grandparent() {
        // `.outer > .target` — `.outer` is two levels up, not direct parent.
        let sheet = s(".outer > .target { color: #fff; }");
        let ancestors = [n("div", &["outer"]), n("div", &["mid"])];
        let r = sheet.resolve(&n("span", &["target"]), &ancestors, &[], None);
        assert!(r.get("color").is_none());
    }

    #[test]
    fn adjacent_sibling_match() {
        let sheet = s(".prev + .target { color: #fff; }");
        let prev_siblings = [n("div", &["prev"])];
        let r = sheet.resolve(&n("span", &["target"]), &[], &prev_siblings, None);
        assert_eq!(
            r.get("color"),
            Some(&Value::Color(Color::rgb(0xff, 0xff, 0xff)))
        );
    }

    #[test]
    fn adjacent_sibling_no_match_non_immediate() {
        // `.prev + .target` — `.prev` is not the *immediately* preceding sibling.
        let sheet = s(".prev + .target { color: #fff; }");
        let prev_siblings = [n("div", &["prev"]), n("div", &["between"])];
        let r = sheet.resolve(&n("span", &["target"]), &[], &prev_siblings, None);
        assert!(r.get("color").is_none());
    }

    #[test]
    fn general_sibling_match_any() {
        let sheet = s(".prev ~ .target { color: #fff; }");
        // `.prev` is not the immediately preceding sibling but still matches `~`.
        let prev_siblings = [n("div", &["prev"]), n("div", &["between"])];
        let r = sheet.resolve(&n("span", &["target"]), &[], &prev_siblings, None);
        assert_eq!(
            r.get("color"),
            Some(&Value::Color(Color::rgb(0xff, 0xff, 0xff)))
        );
    }

    #[test]
    fn general_sibling_no_match_without_sibling() {
        let sheet = s(".prev ~ .target { color: #fff; }");
        let r = sheet.resolve(&n("span", &["target"]), &[], &[], None);
        assert!(r.get("color").is_none());
    }

    #[test]
    fn chained_adjacent_siblings_match() {
        // `.a + .b + .target` against target with prev_siblings = [a, b].
        // Round-2 review caught this false-negativing — the matcher
        // wasn't shrinking `prev_siblings` across consecutive `+`
        // combinators, so the second hop always saw `b` again instead
        // of `a`.
        let sheet = s(".a + .b + .target { color: #fff; }");
        let prev_siblings = [n("div", &["a"]), n("div", &["b"])];
        let r = sheet.resolve(&n("span", &["target"]), &[], &prev_siblings, None);
        assert_eq!(
            r.get("color"),
            Some(&Value::Color(Color::rgb(0xff, 0xff, 0xff)))
        );
    }

    #[test]
    fn chained_general_siblings_match() {
        // `.a ~ .b ~ .target` — `.b` must follow `.a` somewhere in the
        // prev-sibling list, then `.target` follows `.b`.
        let sheet = s(".a ~ .b ~ .target { color: #fff; }");
        let prev_siblings = [n("div", &["a"]), n("div", &["between"]), n("div", &["b"])];
        let r = sheet.resolve(&n("span", &["target"]), &[], &prev_siblings, None);
        assert_eq!(
            r.get("color"),
            Some(&Value::Color(Color::rgb(0xff, 0xff, 0xff)))
        );
    }

    #[test]
    fn specificity_sums_across_parts() {
        // `.a .b.c` — three classes total → specificity 30, not 20.
        let sheet = s(".a .b.c { color: #fff; }");
        let sel = &sheet.rules[0].selectors[0];
        assert_eq!(sel.specificity(), 30);
    }

    #[test]
    fn pseudo_on_ancestor_part_does_not_match() {
        // `.outer:hover > .target` — pseudo applies only to the subject.
        // The ancestor part `.outer:hover` is matched without state (state=None),
        // so `:hover` on it never fires regardless of the target's state.
        let sheet = s(".outer:hover > .target { color: #fff; }");
        let ancestors = [n("div", &["outer"])];
        // Even when the target is in hover state, the rule must not match
        // because the ancestor `.outer` is matched with state=None.
        let r = sheet.resolve(
            &n("span", &["target"]),
            &ancestors,
            &[],
            Some(PseudoClass::Hover),
        );
        assert!(r.get("color").is_none());
    }

    #[test]
    fn explicit_child_combinator_with_whitespace() {
        // `.a > .b` with spaces around `>` must parse as Child.
        let sheet = s(".a > .b { color: #fff; }");
        let sel = &sheet.rules[0].selectors[0];
        assert_eq!(sel.combinators, vec![Combinator::Child]);
    }

    #[test]
    fn explicit_adjacent_sibling_combinator() {
        let sheet = s(".a + .b { color: #fff; }");
        let sel = &sheet.rules[0].selectors[0];
        assert_eq!(sel.combinators, vec![Combinator::AdjacentSibling]);
    }

    #[test]
    fn explicit_general_sibling_combinator() {
        let sheet = s(".a ~ .b { color: #fff; }");
        let sel = &sheet.rules[0].selectors[0];
        assert_eq!(sel.combinators, vec![Combinator::GeneralSibling]);
    }

    // ---- Source-order iter tests ---------------------------------------------

    #[test]
    fn iter_returns_source_order() {
        // Two properties from different rules: iter must yield them in
        // ascending rule_idx order (color rule 0 before background-color
        // rule 1), not alphabetical order (which would also be color first
        // here — use a stronger example with reversed alpha order).
        //
        // "width" (w) sorts after "color" (c) alphabetically, but rule 0
        // sets width and rule 1 sets color, so source order is: width, color.
        let sheet = s(".a { width: 10px; } .a { color: #001; }");
        let r = sheet.resolve(&n("x", &["a"]), &[], &[], None);
        let keys: Vec<&str> = r.iter().map(|(k, _)| k).collect();
        let width_pos = keys.iter().position(|&k| k == "width").unwrap();
        let color_pos = keys.iter().position(|&k| k == "color").unwrap();
        assert!(
            width_pos < color_pos,
            "width (rule 0) must come before color (rule 1): got {keys:?}"
        );
    }

    #[test]
    fn shorthand_then_longhand_source_order() {
        // Case A: border (rule 0) then border-color (rule 1).
        // iter must yield border before border-color.
        let sheet_a = s("x { border: 1px solid red; } x { border-color: blue; }");
        let r_a = sheet_a.resolve(&n("x", &[]), &[], &[], None);
        let keys_a: Vec<&str> = r_a.iter().map(|(k, _)| k).collect();
        let border_pos = keys_a.iter().position(|&k| k == "border").unwrap();
        let bc_pos = keys_a.iter().position(|&k| k == "border-color").unwrap();
        assert!(
            border_pos < bc_pos,
            "border (rule 0) must come before border-color (rule 1): got {keys_a:?}"
        );

        // Case B: reversed — border-color (rule 0) then border (rule 1).
        // iter must yield border-color before border.
        let sheet_b = s("x { border-color: blue; } x { border: 1px solid red; }");
        let r_b = sheet_b.resolve(&n("x", &[]), &[], &[], None);
        let keys_b: Vec<&str> = r_b.iter().map(|(k, _)| k).collect();
        let border_pos_b = keys_b.iter().position(|&k| k == "border").unwrap();
        let bc_pos_b = keys_b.iter().position(|&k| k == "border-color").unwrap();
        assert!(
            bc_pos_b < border_pos_b,
            "border-color (rule 0) must come before border (rule 1): got {keys_b:?}"
        );
    }

    #[test]
    fn intra_rule_source_order() {
        // Two properties declared in the same rule block. Tie-break must
        // be the in-block declaration position, NOT alphabetical.
        // `border-color` declared first, `border` declared second → iter
        // yields border-color before border. An adapter applying these
        // sequentially ends up with the `border` shorthand's color, which
        // is the CSS-correct late-wins semantic.
        let sheet = s("x { border-color: blue; border: 1px solid red; }");
        let r = sheet.resolve(&n("x", &[]), &[], &[], None);
        let keys: Vec<&str> = r.iter().map(|(k, _)| k).collect();
        let bc_pos = keys.iter().position(|&k| k == "border-color").unwrap();
        let border_pos = keys.iter().position(|&k| k == "border").unwrap();
        assert!(
            bc_pos < border_pos,
            "border-color (decl 0) must come before border (decl 1) within the same rule: got {keys:?}"
        );

        // Reversed: border (decl 0), border-color (decl 1) → iter yields
        // border before border-color.
        let sheet_rev = s("x { border: 1px solid red; border-color: blue; }");
        let r_rev = sheet_rev.resolve(&n("x", &[]), &[], &[], None);
        let keys_rev: Vec<&str> = r_rev.iter().map(|(k, _)| k).collect();
        let border_pos_rev = keys_rev.iter().position(|&k| k == "border").unwrap();
        let bc_pos_rev = keys_rev.iter().position(|&k| k == "border-color").unwrap();
        assert!(
            border_pos_rev < bc_pos_rev,
            "border (decl 0) must come before border-color (decl 1) within the same rule: got {keys_rev:?}"
        );
    }
}
