//! Adapter that maps a [`hjkl_css::Stylesheet`] onto floem View styling.
//!
//! Usage:
//!
//! ```rust,ignore
//! use hjkl_css::parse;
//! use hjkl_css_floem::ViewCssExt;
//!
//! let sheet = parse("label.prompt { color: #21d1d3; padding: 4px 8px; }")
//!     .expect("stylesheet parses");
//! let view = floem::views::label(|| "hello")
//!     .css(&sheet, "label", &["prompt"]);
//! ```
//!
//! Use `.css_in(...)` when the view is inside a hierarchy and combinator
//! selectors (` `, `>`, `+`, `~`) need to fire:
//!
//! ```rust,ignore
//! use hjkl_css::{Node, parse};
//! use hjkl_css_floem::ViewCssExt;
//!
//! let sheet = parse(".row .label { color: #fff; }").expect("parses");
//! let target = Node { element: "label", classes: &["prompt"] };
//! let ancestors = [Node { element: "row", classes: &[] }];
//! let view = floem::views::label(|| "hello")
//!     .css_in(&sheet, target, &ancestors, &[]);
//! ```
//!
//! The trait resolves the stylesheet eagerly for the base state and each
//! supported pseudo (`:hover`, `:focus`, `:active`, `:disabled`,
//! `:selected`), capturing the resolved property bags into the floem
//! `.style(|s| ...)` closure. Pseudo-state blocks delegate to floem's
//! own `.hover()` / `.focus()` / etc. chain so floem handles the
//! interaction wiring.
//!
//! # Workspace setup
//!
//! floem's Wayland layer-shell support lives on a fork (`mxaddict/floem`,
//! `layer-shell` branch) that requires a patched `floem-winit`. Cargo
//! only honours `[patch.crates-io]` declared at the workspace root, so
//! `hjkl-css-floem` cannot ship the patch transitively. Downstream
//! consumers building on Wayland with layer-shell **must** add the
//! following to their own workspace `Cargo.toml`:
//!
//! ```toml
//! [patch.crates-io]
//! floem       = { git = "https://github.com/mxaddict/floem.git", branch = "layer-shell" }
//! floem-winit = { git = "https://github.com/mxaddict/winit.git", branch = "layer-shell" }
//! ```
//!
//! Without this block the build succeeds against stock `floem 0.2` from
//! crates.io — there is no compile-time signal — and the layer-shell
//! features fail silently at runtime.

use floem::peniko::{Brush, Color};
use floem::style::Style;
use floem::taffy::style::{AlignItems, Display, FlexDirection, JustifyContent};
use floem::text::Weight;
use floem::unit::{Px, PxPct, PxPctAuto};
use floem::views::Decorators;
use hjkl_css::{
    Length, Node, PseudoClass, ResolvedStyle, SideValue, Stylesheet, Value, expand_side_set,
    expand_sides,
};

/// Extension trait that consumes a [`Stylesheet`] and applies its
/// resolved properties — base plus every supported pseudo-state — to a
/// floem view in one call.
///
/// Two entry points are provided:
///
/// - `.css(sheet, element, classes)` — flat call for top-level views with no
///   combinator context. Builds a [`Node`] internally and calls `.css_in` with
///   empty ancestor / sibling slices.
///
/// - `.css_in(sheet, target, ancestors, prev_siblings)` — context-aware call
///   for views that live inside a hierarchy. Enables descendant (` `), child
///   (`>`), adjacent-sibling (`+`), and general-sibling (`~`) selectors to
///   match.
///
/// A `CssContext` builder is deliberately *not* introduced here: the two-method
/// surface keeps the API minimal. If a third variant is needed in the future
/// (e.g. per-property filtering or lazy resolution), `CssContext` is the right
/// abstraction to reach for then.
///
/// **Sealed by the blanket impl.** Downstream crates cannot add their own
/// `impl ViewCssExt for SomeWrapper` because the orphan rule + the blanket impl
/// would conflict. Consumers needing custom property application should write a
/// free function or their own extension trait that delegates to this one.
pub trait ViewCssExt: Decorators + Sized {
    /// Resolve and apply styles for `element` with `classes`, no combinator
    /// context. Equivalent to `.css_in(sheet, Node { element, classes },
    /// &[], &[])`.
    #[must_use = "css() returns a styled view; bind it or chain further"]
    fn css(self, sheet: &Stylesheet, element: &str, classes: &[&str]) -> Self::DV {
        let target = Node { element, classes };
        self.css_in(sheet, target, &[], &[])
    }

    /// Resolve and apply styles for `target` inside a given tree context.
    /// `ancestors` is root→parent (exclusive of target); `prev_siblings` is
    /// oldest→immediately-preceding sibling (exclusive of target).
    #[must_use = "css_in() returns a styled view; bind it or chain further"]
    fn css_in(
        self,
        sheet: &Stylesheet,
        target: Node<'_>,
        ancestors: &[Node<'_>],
        prev_siblings: &[Node<'_>],
    ) -> Self::DV {
        let states = StateStyles::resolve(sheet, target, ancestors, prev_siblings);
        self.style(move |s| {
            let mut s = apply(s, &states.base);
            s = s.hover(|hs| apply(hs, &states.hover));
            s = s.focus(|fs| apply(fs, &states.focus));
            s = s.active(|act| apply(act, &states.active));
            s = s.disabled(|ds| apply(ds, &states.disabled));
            s = s.selected(|sel| apply(sel, &states.selected));
            s
        })
    }
}

impl<V: Decorators + Sized> ViewCssExt for V {}

/// Pre-resolved property bags for each state. Eager resolution avoids
/// running the cascade on every floem style closure invocation.
///
/// floem's `.style(...)` takes `impl Fn(Style) -> Style` and re-invokes
/// the closure whenever any captured reactive signal changes. The
/// adapter closure accesses each state bag by reference (`&states.base`,
/// `&states.hover`, …) so the move-captured `StateStyles` is reused
/// across invocations without needing to be cloned. `Clone` is kept for
/// defensive completeness — useful if a caller wants to fork the bag
/// for inspection or composition — but is not required for the
/// `.style(...)` call path.
#[derive(Clone)]
struct StateStyles {
    base: ResolvedStyle,
    hover: ResolvedStyle,
    focus: ResolvedStyle,
    active: ResolvedStyle,
    disabled: ResolvedStyle,
    selected: ResolvedStyle,
}

impl StateStyles {
    fn resolve(
        sheet: &Stylesheet,
        target: Node<'_>,
        ancestors: &[Node<'_>],
        prev_siblings: &[Node<'_>],
    ) -> Self {
        Self {
            base: sheet.resolve(&target, ancestors, prev_siblings, None),
            hover: sheet.resolve(&target, ancestors, prev_siblings, Some(PseudoClass::Hover)),
            focus: sheet.resolve(&target, ancestors, prev_siblings, Some(PseudoClass::Focus)),
            active: sheet.resolve(&target, ancestors, prev_siblings, Some(PseudoClass::Active)),
            disabled: sheet.resolve(
                &target,
                ancestors,
                prev_siblings,
                Some(PseudoClass::Disabled),
            ),
            selected: sheet.resolve(
                &target,
                ancestors,
                prev_siblings,
                Some(PseudoClass::Selected),
            ),
        }
    }
}

/// Walk every property in `resolved` and chain the matching floem
/// `Style` setter. Unknown properties (or values of the wrong shape for
/// a known property — though the parser already filters most of those)
/// are silently skipped.
fn apply(mut s: Style, resolved: &ResolvedStyle) -> Style {
    for (prop, value) in resolved.iter() {
        s = apply_one(s, prop, value);
    }
    s
}

#[allow(clippy::too_many_lines)]
fn apply_one(s: Style, prop: &str, value: &Value) -> Style {
    match prop {
        // ── Color ─────────────────────────────────────────────────────────────
        "color" => match value {
            Value::Color(c) => s.color(to_peniko_color(*c)),
            _ => s,
        },
        "background-color" => match value {
            Value::Color(c) => s.background(to_peniko_color(*c)),
            _ => s,
        },

        // ── Sizing ────────────────────────────────────────────────────────────
        "width" => match value {
            Value::Length(l) => s.width(to_pct_auto(*l)),
            Value::Auto => s.width(PxPctAuto::Auto),
            _ => s,
        },
        "height" => match value {
            Value::Length(l) => s.height(to_pct_auto(*l)),
            Value::Auto => s.height(PxPctAuto::Auto),
            _ => s,
        },
        "flex-basis" => match value {
            Value::Length(l) => s.flex_basis(to_pct_auto(*l)),
            Value::Auto => s.flex_basis(PxPctAuto::Auto),
            _ => s,
        },

        // ── Box spacing ───────────────────────────────────────────────────────
        "padding" | "border-radius" => apply_padding_or_border_radius(s, prop, value),
        "margin" => apply_margin(s, value),
        "gap" => match value {
            Value::Length(l) => s.gap(to_pct(*l)),
            _ => s,
        },
        "row-gap" => match value {
            Value::Length(l) => s.row_gap(to_pct(*l)),
            _ => s,
        },
        "column-gap" => match value {
            Value::Length(l) => s.column_gap(to_pct(*l)),
            _ => s,
        },

        // ── Layout ────────────────────────────────────────────────────────────
        "display" => match value {
            Value::Keyword(kw) => match kw.as_str() {
                "flex" => s.display(Display::Flex),
                "block" => s.display(Display::Block),
                "none" => s.display(Display::None),
                _ => s,
            },
            _ => s,
        },
        "flex-direction" => match value {
            Value::Keyword(kw) => match kw.as_str() {
                "row" => s.flex_direction(FlexDirection::Row),
                "column" => s.flex_direction(FlexDirection::Column),
                "row-reverse" => s.flex_direction(FlexDirection::RowReverse),
                "column-reverse" => s.flex_direction(FlexDirection::ColumnReverse),
                _ => s,
            },
            _ => s,
        },
        "align-items" => match value {
            Value::Keyword(kw) => match kw.as_str() {
                "start" => s.align_items(Some(AlignItems::Start)),
                "end" => s.align_items(Some(AlignItems::End)),
                "center" => s.align_items(Some(AlignItems::Center)),
                "stretch" => s.align_items(Some(AlignItems::Stretch)),
                "baseline" => s.align_items(Some(AlignItems::Baseline)),
                _ => s,
            },
            _ => s,
        },
        "justify-content" => match value {
            Value::Keyword(kw) => match kw.as_str() {
                "start" => s.justify_content(Some(JustifyContent::Start)),
                "end" => s.justify_content(Some(JustifyContent::End)),
                "center" => s.justify_content(Some(JustifyContent::Center)),
                "space-between" => s.justify_content(Some(JustifyContent::SpaceBetween)),
                "space-around" => s.justify_content(Some(JustifyContent::SpaceAround)),
                "space-evenly" => s.justify_content(Some(JustifyContent::SpaceEvenly)),
                _ => s,
            },
            _ => s,
        },
        "flex-grow" => match value {
            Value::Number(n) => s.flex_grow(*n as f32),
            _ => s,
        },
        "flex-shrink" => match value {
            Value::Number(n) => s.flex_shrink(*n as f32),
            _ => s,
        },

        // ── Border shorthands ─────────────────────────────────────────────────
        "border" => apply_border(s, value, BorderSide::All),
        "border-top" => apply_border(s, value, BorderSide::Top),
        "border-right" => apply_border(s, value, BorderSide::Right),
        "border-bottom" => apply_border(s, value, BorderSide::Bottom),
        "border-left" => apply_border(s, value, BorderSide::Left),
        // `outline` maps to floem's outline + outline_color setters.
        "outline" => match value {
            Value::Border { width, color } => {
                let Some(px) = width.as_px() else { return s };
                s.outline(px).outline_color(to_peniko_brush(*color))
            }
            _ => s,
        },
        // `border-width` is a 1..=4 side shorthand — set all four sides.
        "border-width" => apply_border_width(s, value),
        "border-color" => match value {
            Value::Color(c) => s.border_color(to_peniko_brush(*c)),
            _ => s,
        },
        // floem 0.2 has a single global border_color brush; per-side colors
        // collapse to last-write-wins (see apply_border doc for details).
        "border-top-color" | "border-right-color" | "border-bottom-color" | "border-left-color" => {
            match value {
                Value::Color(c) => s.border_color(to_peniko_brush(*c)),
                _ => s,
            }
        }

        // ── Typography ────────────────────────────────────────────────────────
        "font-size" => match value {
            Value::Length(l) => {
                if let Some(px) = l.as_px() {
                    s.font_size(Px(px))
                } else {
                    // Percent font-sizes have no direct floem setter.
                    // Gap: floem 0.2 font_size takes Px only, no % support.
                    s
                }
            }
            _ => s,
        },
        "font-weight" => match value {
            Value::Number(n) => s.font_weight(Weight(*n as u16)),
            Value::Keyword(kw) => match kw.as_str() {
                "normal" => s.font_weight(Weight::NORMAL),
                "bold" => s.font_weight(Weight::BOLD),
                _ => s,
            },
            _ => s,
        },
        "font-style" => match value {
            Value::Keyword(kw) => match kw.as_str() {
                "italic" => s.font_style(floem::text::Style::Italic),
                "oblique" => s.font_style(floem::text::Style::Oblique),
                "normal" => s.font_style(floem::text::Style::Normal),
                _ => s,
            },
            _ => s,
        },
        "font-family" => match value {
            // Use the first family name; floem 0.2 font_family takes a
            // single String. A future floem version may accept a list.
            Value::FontFamilyList(list) => {
                if let Some(first) = list.first() {
                    s.font_family(first.clone())
                } else {
                    s
                }
            }
            _ => s,
        },
        "line-height" => match value {
            Value::Number(n) => s.line_height(*n as f32),
            // Gap: floem 0.2 line_height takes f32 (normal multiplier); no
            // pixel-value setter is exposed. Length variant is silently
            // skipped.
            _ => s,
        },
        // Gap: floem 0.2 has no text_align setter. The CSS property is
        // parsed and resolved by hjkl-css but cannot be forwarded.
        "text-align" => s,

        _ => s,
    }
}

// ── Border helpers ────────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
enum BorderSide {
    All,
    Top,
    Right,
    Bottom,
    Left,
}

/// Apply a `Value::Border` (width + color) to the requested side(s).
///
/// **Per-side color limitation.** floem 0.2 exposes a single global
/// `border_color` brush — there is no per-side border color. When the CSS
/// declares `border-top: 2px solid red; border-left: 2px solid blue`, the
/// adapter calls `border_color(red)` then `border_color(blue)` and the
/// last write wins for every side. This is a floem limitation surfaced
/// here, not a parser bug: the resolved bag contains distinct colors,
/// but the renderer cannot honour them. Authors targeting v0.x should
/// either keep all per-side colors identical or set one global
/// `border-color`.
///
/// **Source order honoured.** hjkl-css v0.4.0's `iter()` returns properties
/// in CSS source order, so the adapter walks properties in the order they
/// appear in the stylesheet. Shorthand (`border`) and longhand
/// (`border-color`) are applied in the same sequence the author wrote them —
/// no more alphabetical-order surprises.
///
/// **Always emits `border_color`.** Every `border` / `border-{side}` shorthand
/// declaration emits both a width call and a `border_color` call, even when
/// the author wrote only a width-and-style pair. This is intentional: a
/// shorthand resets all its sub-properties per CSS spec. If a later
/// `border-color` longhand follows in source order, it overrides via the
/// single-brush model documented above.
fn apply_border(s: Style, value: &Value, side: BorderSide) -> Style {
    let Value::Border { width, color } = value else {
        return s;
    };
    let Some(px) = width.as_px() else { return s };
    let s = match side {
        BorderSide::All => s.border(px),
        BorderSide::Top => s.border_top(px),
        BorderSide::Right => s.border_right(px),
        BorderSide::Bottom => s.border_bottom(px),
        BorderSide::Left => s.border_left(px),
    };
    s.border_color(to_peniko_brush(*color))
}

fn apply_border_width(s: Style, value: &Value) -> Style {
    let Value::LengthSet(set) = value else {
        return s;
    };
    let Some([top, right, bottom, left]) = expand_sides(set) else {
        return s;
    };
    let (t, r, b, l) = (top.as_px(), right.as_px(), bottom.as_px(), left.as_px());
    let Some((t, r, b, l)) = t
        .zip(r)
        .and_then(|(t, r)| b.zip(l).map(|(b, l)| (t, r, b, l)))
    else {
        // Percent border widths have no floem equivalent; skip.
        return s;
    };
    s.border_top(t)
        .border_right(r)
        .border_bottom(b)
        .border_left(l)
}

// ── Padding / border-radius helper ────────────────────────────────────────────

fn apply_padding_or_border_radius(s: Style, prop: &str, value: &Value) -> Style {
    let Value::LengthSet(set) = value else {
        return s;
    };
    let Some([top, right, bottom, left]) = expand_sides(set) else {
        return s;
    };
    if prop == "border-radius" {
        // floem 0.2 exposes a single border_radius(PxPct) — use the top
        // value (first in the shorthand) as the uniform radius.
        // Gap: floem 0.2 has no per-corner border-radius setters.
        s.border_radius(to_pct(top))
    } else {
        s.padding_top(to_pct(top))
            .padding_right(to_pct(right))
            .padding_bottom(to_pct(bottom))
            .padding_left(to_pct(left))
    }
}

// ── Margin helper ─────────────────────────────────────────────────────────────

fn apply_margin(s: Style, value: &Value) -> Style {
    match value {
        Value::LengthSet(set) => {
            let Some([top, right, bottom, left]) = expand_sides(set) else {
                return s;
            };
            s.margin_top(to_pct_auto(top))
                .margin_right(to_pct_auto(right))
                .margin_bottom(to_pct_auto(bottom))
                .margin_left(to_pct_auto(left))
        }
        Value::Auto => s
            .margin_top(PxPctAuto::Auto)
            .margin_right(PxPctAuto::Auto)
            .margin_bottom(PxPctAuto::Auto)
            .margin_left(PxPctAuto::Auto),
        Value::SideSet(set) => {
            let Some([top, right, bottom, left]) = expand_side_set(set) else {
                return s;
            };
            s.margin_top(to_pct_auto_side(top))
                .margin_right(to_pct_auto_side(right))
                .margin_bottom(to_pct_auto_side(bottom))
                .margin_left(to_pct_auto_side(left))
        }
        _ => s,
    }
}

// ── Conversion helpers ────────────────────────────────────────────────────────

fn to_peniko_color(c: hjkl_css::Color) -> Color {
    Color::rgba8(c.r, c.g, c.b, c.a)
}

fn to_peniko_brush(c: hjkl_css::Color) -> Brush {
    Brush::Solid(to_peniko_color(c))
}

fn to_pct(l: Length) -> PxPct {
    match l {
        Length::Px(v) => PxPct::Px(v),
        Length::Percent(v) => PxPct::Pct(v),
    }
}

fn to_pct_auto(l: Length) -> PxPctAuto {
    match l {
        Length::Px(v) => PxPctAuto::Px(v),
        Length::Percent(v) => PxPctAuto::Pct(v),
    }
}

/// Convert a single `SideValue` to `PxPctAuto` for margin shorthands that
/// mix lengths with `auto` (e.g. `margin: 4px auto`).
fn to_pct_auto_side(v: SideValue) -> PxPctAuto {
    match v {
        SideValue::Length(l) => to_pct_auto(l),
        SideValue::Auto => PxPctAuto::Auto,
    }
}

#[cfg(test)]
mod tests {
    use hjkl_css::{Color, Node, PseudoClass, Value};

    use super::*;

    // ── Conversion helpers ────────────────────────────────────────────────────

    /// Smoke test: every conversion helper round-trips a representative
    /// value without panicking. Compile-time evidence that the hjkl-css
    /// AST types line up with floem's expected input shapes.
    #[test]
    fn conversions_are_total() {
        let c = to_peniko_color(hjkl_css::Color::rgba(0x21, 0xd1, 0xd3, 0xff));
        assert_eq!((c.r, c.g, c.b, c.a), (0x21, 0xd1, 0xd3, 0xff));
        assert!(matches!(to_pct(Length::Px(10.0)), PxPct::Px(_)));
        assert!(matches!(to_pct(Length::Percent(50.0)), PxPct::Pct(_)));
        assert!(matches!(to_pct_auto(Length::Px(10.0)), PxPctAuto::Px(_)));
        assert!(matches!(
            to_pct_auto(Length::Percent(50.0)),
            PxPctAuto::Pct(_)
        ));
        assert!(matches!(to_pct_auto_side(SideValue::Auto), PxPctAuto::Auto));
        assert!(matches!(
            to_pct_auto_side(SideValue::Length(Length::Px(4.0))),
            PxPctAuto::Px(_)
        ));
    }

    /// Resolving a stylesheet for an empty class list against a property
    /// nobody set is a no-op — apply returns the input Style untouched.
    #[test]
    fn empty_resolved_does_not_panic() {
        let sheet = hjkl_css::parse(".unrelated { color: #fff; }").unwrap();
        let target = Node {
            element: "nothing",
            classes: &[],
        };
        let resolved = sheet.resolve(&target, &[], &[], None);
        let s = Style::new();
        let _ = apply(s, &resolved);
    }

    // ── Full property surface — no panic ─────────────────────────────────────

    /// Parse a stylesheet that exercises every Value variant the parser can
    /// emit, resolve it, and run apply() — confirming no panics across the
    /// full property surface.
    #[test]
    fn all_value_variants_apply_without_panic() {
        let css = r#"
            x {
                color: #ff0000;
                background-color: rgba(0, 128, 0, 0.5);
                width: 100px;
                height: 50%;
                width: auto;
                padding: 4px 8px;
                margin: 4px auto;
                gap: 8px;
                row-gap: 4px;
                column-gap: 2px;
                display: flex;
                flex-direction: column;
                align-items: center;
                justify-content: space-between;
                flex-grow: 2;
                flex-shrink: 0;
                flex-basis: 200px;
                border: 1px solid #000;
                border-top: 2px solid #fff;
                border-right: 2px solid #fff;
                border-bottom: 2px solid #fff;
                border-left: 2px solid #fff;
                border-width: 1px 2px 3px 4px;
                border-color: blue;
                border-radius: 4px;
                outline: 1px solid #000;
                font-size: 16px;
                font-weight: 700;
                font-weight: bold;
                font-style: italic;
                font-family: "Hack Nerd Font", monospace;
                line-height: 1.5;
                text-align: center;
            }
        "#;
        let sheet = hjkl_css::parse(css).unwrap();
        let target = Node {
            element: "x",
            classes: &[],
        };
        let resolved = sheet.resolve(&target, &[], &[], None);
        // Must not panic; result is discarded.
        let _ = apply(Style::new(), &resolved);
    }

    // ── Combinator: descendant selector ──────────────────────────────────────

    /// `.parent .child { color: red }` resolved with a matching ancestor sets
    /// the property; resolved without ancestors does not.
    #[test]
    fn descendant_combinator_with_ancestors_sets_property() {
        let sheet = hjkl_css::parse(".parent .child { color: #ff0000; }").unwrap();
        let child = Node {
            element: "div",
            classes: &["child"],
        };
        let parent = Node {
            element: "div",
            classes: &["parent"],
        };

        // With matching ancestor — must resolve.
        let with_ancestor = sheet.resolve(&child, &[parent], &[], None);
        assert_eq!(
            with_ancestor.get("color"),
            Some(&Value::Color(Color::rgb(0xff, 0x00, 0x00))),
            "expected color when ancestor matches"
        );

        // Without ancestors — must not resolve.
        let without_ancestor = sheet.resolve(&child, &[], &[], None);
        assert!(
            without_ancestor.get("color").is_none(),
            "must not match without ancestor"
        );
    }

    // ── Pseudo-state buckets ─────────────────────────────────────────────────

    /// `:hover` declarations land only in the hover-state bag.
    #[test]
    fn hover_declaration_only_in_hover_state() {
        let sheet = hjkl_css::parse(".btn { color: #000; } .btn:hover { color: #fff; }").unwrap();
        let target = Node {
            element: "button",
            classes: &["btn"],
        };

        let base = sheet.resolve(&target, &[], &[], None);
        let hover = sheet.resolve(&target, &[], &[], Some(PseudoClass::Hover));

        assert_eq!(
            base.get("color"),
            Some(&Value::Color(Color::rgb(0x00, 0x00, 0x00))),
            "base must use non-pseudo color"
        );
        assert_eq!(
            hover.get("color"),
            Some(&Value::Color(Color::rgb(0xff, 0xff, 0xff))),
            "hover must use :hover color"
        );
    }

    /// `:disabled` declarations land only in the disabled-state bag.
    #[test]
    fn disabled_declaration_only_in_disabled_state() {
        let sheet = hjkl_css::parse(".btn:disabled { color: #aaa; }").unwrap();
        let target = Node {
            element: "button",
            classes: &["btn"],
        };

        let base = sheet.resolve(&target, &[], &[], None);
        let disabled = sheet.resolve(&target, &[], &[], Some(PseudoClass::Disabled));

        assert!(
            base.get("color").is_none(),
            "base must not have :disabled color"
        );
        assert_eq!(
            disabled.get("color"),
            Some(&Value::Color(Color::rgb(0xaa, 0xaa, 0xaa))),
            "disabled must use :disabled color"
        );
    }

    // ── margin: auto and SideSet ─────────────────────────────────────────────

    /// `margin: auto` must not panic and must reach the Auto branch.
    #[test]
    fn margin_auto_applies_without_panic() {
        let sheet = hjkl_css::parse("x { margin: auto; }").unwrap();
        let target = Node {
            element: "x",
            classes: &[],
        };
        let resolved = sheet.resolve(&target, &[], &[], None);
        // Confirm the parser emits Auto for margin: auto.
        assert_eq!(resolved.get("margin"), Some(&Value::Auto));
        let _ = apply(Style::new(), &resolved);
    }

    /// `margin: 4px auto` → SideSet, apply must not panic.
    #[test]
    fn margin_side_set_applies_without_panic() {
        let sheet = hjkl_css::parse("x { margin: 4px auto; }").unwrap();
        let target = Node {
            element: "x",
            classes: &[],
        };
        let resolved = sheet.resolve(&target, &[], &[], None);
        assert!(
            matches!(resolved.get("margin"), Some(Value::SideSet(_))),
            "expected SideSet for mixed margin"
        );
        let _ = apply(Style::new(), &resolved);
    }

    // ── Border: per-side colors collide (documented floem limitation) ────────

    /// floem 0.2 has a single `border_color` brush, so per-side CSS colors
    /// silently unify to last-write-wins. Test guarantees `apply` does not
    /// panic on mixed per-side colors and pins the limitation under test —
    /// if a future floem gains per-side colors, this test should be
    /// re-expressed to assert per-side preservation.
    #[test]
    fn per_side_border_colors_resolve_without_panic() {
        let css = r#"
            x {
                border-top: 2px solid #ff0000;
                border-left: 2px solid #0000ff;
            }
        "#;
        let sheet = hjkl_css::parse(css).unwrap();
        let target = Node {
            element: "x",
            classes: &[],
        };
        let resolved = sheet.resolve(&target, &[], &[], None);
        // Both declarations resolve into the bag; only the renderer collapses.
        assert!(resolved.get("border-top").is_some());
        assert!(resolved.get("border-left").is_some());
        let _ = apply(Style::new(), &resolved);
    }

    // ── flex-basis routes to flex_basis, not width ───────────────────────────

    /// Regression: `flex-basis` must resolve to its own bag entry, not get
    /// silently re-mapped to `width`. Pins the routing contract — actual
    /// floem setter selection is verified at compile time by `apply_one`.
    #[test]
    fn flex_basis_resolves_independently_from_width() {
        let sheet = hjkl_css::parse("x { flex-basis: 200px; }").unwrap();
        let target = Node {
            element: "x",
            classes: &[],
        };
        let resolved = sheet.resolve(&target, &[], &[], None);
        assert_eq!(
            resolved.get("flex-basis"),
            Some(&Value::Length(Length::Px(200.0)))
        );
        assert!(resolved.get("width").is_none(), "must not also set width");
        let _ = apply(Style::new(), &resolved);
    }

    // ── css_in API smoke test ────────────────────────────────────────────────

    /// StateStyles::resolve accepts a non-empty ancestor slice without panic.
    #[test]
    fn state_styles_resolve_with_ancestors() {
        let sheet = hjkl_css::parse(".row .label { color: #fff; }").unwrap();
        let target = Node {
            element: "span",
            classes: &["label"],
        };
        let row = Node {
            element: "div",
            classes: &["row"],
        };
        let states = StateStyles::resolve(&sheet, target, &[row], &[]);
        // base must have color resolved via the descendant combinator.
        assert_eq!(
            states.base.get("color"),
            Some(&Value::Color(Color::rgb(0xff, 0xff, 0xff)))
        );
        // Resolving with empty ancestors must yield nothing.
        let states_no_ctx = StateStyles::resolve(&sheet, target, &[], &[]);
        assert!(states_no_ctx.base.is_empty());
    }

    // ── font-style: oblique ──────────────────────────────────────────────────

    /// `font-style: oblique` resolves to a Keyword value and apply runs
    /// without panic. The Oblique → `floem::text::Style::Oblique` routing is
    /// checked at compile time via the match arm; floem 0.2 exposes no
    /// `Style` introspection API so the resulting setter call cannot be
    /// asserted at runtime.
    #[test]
    fn font_style_oblique_resolves() {
        let sheet = hjkl_css::parse("x { font-style: oblique; }").unwrap();
        let target = Node {
            element: "x",
            classes: &[],
        };
        let resolved = sheet.resolve(&target, &[], &[], None);
        assert_eq!(
            resolved.get("font-style"),
            Some(&Value::Keyword("oblique".into())),
            "expected oblique keyword from parser"
        );
        let _ = apply(Style::new(), &resolved);
    }

    // ── border per-side color longhands ──────────────────────────────────────

    /// Each of the four per-side border color longhands resolves to
    /// `Value::Color` in hjkl-css v0.4.0 and apply runs without panic.
    /// floem 0.2 collapses all four to a single brush (last-write-wins).
    #[test]
    fn border_side_color_longhands_resolve() {
        let css = r#"
            x {
                border-top-color: #ff0000;
                border-right-color: #00ff00;
                border-bottom-color: #0000ff;
                border-left-color: #ffff00;
            }
        "#;
        let sheet = hjkl_css::parse(css).unwrap();
        let target = Node {
            element: "x",
            classes: &[],
        };
        let resolved = sheet.resolve(&target, &[], &[], None);
        assert!(
            matches!(resolved.get("border-top-color"), Some(Value::Color(_))),
            "border-top-color must resolve to Color"
        );
        assert!(
            matches!(resolved.get("border-right-color"), Some(Value::Color(_))),
            "border-right-color must resolve to Color"
        );
        assert!(
            matches!(resolved.get("border-bottom-color"), Some(Value::Color(_))),
            "border-bottom-color must resolve to Color"
        );
        assert!(
            matches!(resolved.get("border-left-color"), Some(Value::Color(_))),
            "border-left-color must resolve to Color"
        );
        // apply must not panic even though floem collapses to last-write-wins.
        let _ = apply(Style::new(), &resolved);
    }

    // ── Integration: .css() and .css_in() compile and run without panic ──────

    /// Approach (a): construct a label, stack, and container view, chain
    /// `.css(...)` and `.css_in(...)` on each, and confirm the calls
    /// typecheck and do not panic. No headless floem runtime is required —
    /// the style closure is captured but not driven by a reactor.
    ///
    /// **Coverage scope.** This test catches compile-time regressions in
    /// the `ViewCssExt` trait surface and the `apply_one` dispatch
    /// signature. It does NOT catch runtime routing mistakes (e.g. a
    /// width property accidentally routed to a height setter) because
    /// floem 0.2 exposes no `Style` introspection API to assert on. Per-
    /// property routing correctness is enforced by the per-property
    /// resolver tests above and by visual inspection in adopter crates.
    #[test]
    fn integration_label_view_with_css() {
        use hjkl_css::Node as CssNode;

        let sheet = hjkl_css::parse(
            r#"
            label { color: #21d1d3; padding: 4px 8px; font-style: oblique; }
            label.prompt { font-weight: bold; }
            .row label { color: #ffffff; }
            "#,
        )
        .unwrap();

        // Flat call: label with a class.
        let _label = floem::views::label(|| "hello").css(&sheet, "label", &["prompt"]);

        // Context-aware call: label inside a row.
        let row = CssNode {
            element: "div",
            classes: &["row"],
        };
        let target = CssNode {
            element: "label",
            classes: &[],
        };
        let _label_in = floem::views::label(|| "world").css_in(&sheet, target, &[row], &[]);

        // Stack with css.
        let _stack = floem::views::stack((floem::views::label(|| "a"),)).css(&sheet, "stack", &[]);

        // Container with css.
        let _container =
            floem::views::container(floem::views::label(|| "b")).css(&sheet, "container", &[]);
    }
}
