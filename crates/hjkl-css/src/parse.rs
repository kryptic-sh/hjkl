//! Convert a CSS-subset string to a [`Stylesheet`]. Backed by `cssparser`'s
//! tokenizer + `StyleSheetParser`. Compound selectors with descendant (` `),
//! child (`>`), adjacent-sibling (`+`), and general-sibling (`~`) combinators
//! are supported.

use cssparser::{
    AtRuleParser, CowRcStr, DeclarationParser, ParseError as CssParseError, Parser, ParserInput,
    ParserState, QualifiedRuleParser, RuleBodyItemParser, RuleBodyParser, StyleSheetParser, Token,
    match_ignore_ascii_case,
};

use crate::ast::{
    Combinator, Declaration, PseudoClass, Rule, Selector, SimpleSelector, Stylesheet,
};
use crate::error::{ParseError, ParseErrorOwned};
use crate::value::{Color, Length, SideValue, Value};

pub fn parse(input: &str) -> Result<Stylesheet, ParseError> {
    let mut parser_input = ParserInput::new(input);
    let mut parser = Parser::new(&mut parser_input);
    let mut rule_parser = StylesheetRuleParser;
    let mut rules = Vec::new();
    let iter = StyleSheetParser::new(&mut parser, &mut rule_parser);
    for item in iter {
        match item {
            Ok(Some(rule)) => rules.push(rule),
            // `None` here is an at-rule we chose to swallow (e.g. `@media`,
            // `@charset`) — keep parsing, don't surface as an error.
            Ok(None) => {}
            // CSS spec: a single malformed rule must not invalidate the
            // surrounding stylesheet. cssparser already skips the broken
            // rule's tokens before yielding the next item, so we drop the
            // error and keep collecting.
            Err(_) => {}
        }
    }
    Ok(Stylesheet { rules })
}

struct StylesheetRuleParser;

impl<'i> QualifiedRuleParser<'i> for StylesheetRuleParser {
    type Prelude = Vec<Selector>;
    type QualifiedRule = Option<Rule>;
    type Error = ParseErrorOwned;

    fn parse_prelude<'t>(
        &mut self,
        parser: &mut Parser<'i, 't>,
    ) -> Result<Self::Prelude, CssParseError<'i, Self::Error>> {
        parse_selector_list(parser)
    }

    fn parse_block<'t>(
        &mut self,
        selectors: Self::Prelude,
        _start: &ParserState,
        parser: &mut Parser<'i, 't>,
    ) -> Result<Self::QualifiedRule, CssParseError<'i, Self::Error>> {
        let mut declarations = Vec::new();
        let mut decl_parser = DeclParser;
        let body = RuleBodyParser::new(parser, &mut decl_parser);
        // CSS spec: a malformed declaration must not invalidate the
        // surrounding rule. cssparser already advances past the broken
        // declaration; we just swallow the error and keep collecting the
        // rest.
        for item in body {
            match item {
                Ok(decl) => declarations.push(decl),
                Err(_) => continue,
            }
        }
        Ok(Some(Rule {
            selectors,
            declarations,
        }))
    }
}

impl<'i> AtRuleParser<'i> for StylesheetRuleParser {
    type Prelude = ();
    type AtRule = Option<Rule>;
    type Error = ParseErrorOwned;

    // Consume an at-rule prelude (everything up to `;` or `{`) and discard
    // it. Returning Ok signals "recognized"; the matching parse_block
    // (for block at-rules) or rule-list parser (for statement at-rules)
    // will skip the body. v1 doesn't implement any at-rule semantics, but
    // we swallow them so a real-world stylesheet with `@charset` /
    // `@media` doesn't blow up the whole parse.
    fn parse_prelude<'t>(
        &mut self,
        _name: CowRcStr<'i>,
        parser: &mut Parser<'i, 't>,
    ) -> Result<Self::Prelude, CssParseError<'i, Self::Error>> {
        while parser.next().is_ok() {}
        Ok(())
    }

    fn rule_without_block(
        &mut self,
        _prelude: Self::Prelude,
        _start: &ParserState,
    ) -> Result<Self::AtRule, ()> {
        Ok(None)
    }

    fn parse_block<'t>(
        &mut self,
        _prelude: Self::Prelude,
        _start: &ParserState,
        parser: &mut Parser<'i, 't>,
    ) -> Result<Self::AtRule, CssParseError<'i, Self::Error>> {
        // Drain the block — its contents (nested rules or declarations)
        // are intentionally discarded in v1.
        while parser.next().is_ok() {}
        Ok(None)
    }
}

fn parse_selector_list<'i, 't>(
    parser: &mut Parser<'i, 't>,
) -> Result<Vec<Selector>, CssParseError<'i, ParseErrorOwned>> {
    let mut selectors = Vec::new();
    loop {
        selectors.push(parse_compound_selector(parser)?);
        if parser.expect_comma().is_err() {
            break;
        }
    }
    Ok(selectors)
}

/// Parse one [`SimpleSelector`] from the token stream. Consumes only
/// tokens that belong to the simple selector (element, classes, pseudo).
/// Stops — without consuming — at whitespace or any token that cannot
/// continue the current simple selector.
///
/// `allow_type` controls whether a leading `Ident` token is accepted as a
/// type selector. The very first part of a compound selector allows a type
/// selector; subsequent parts (after an explicit combinator) also allow one
/// since the combinator was already consumed. The only case that disallows
/// a type selector is a non-first part reached via the whitespace/descendant
/// path, where the Ident has already been put back by the caller.
fn parse_simple_selector<'i, 't>(
    parser: &mut Parser<'i, 't>,
    allow_type: bool,
) -> Result<SimpleSelector, CssParseError<'i, ParseErrorOwned>> {
    let mut sel = SimpleSelector::default();
    let mut saw_anything = false;
    loop {
        let save = parser.state();
        let next = parser.next_including_whitespace().cloned();
        match next {
            Ok(Token::Ident(name)) if allow_type && !saw_anything => {
                sel.element = Some(name.to_string());
                saw_anything = true;
            }
            Ok(Token::Delim('.')) => {
                let name = parser.expect_ident()?.to_string();
                sel.classes.push(name);
                saw_anything = true;
            }
            Ok(Token::Colon) => {
                let ident = parser.expect_ident_cloned()?;
                let pseudo = match PseudoClass::from_ident(&ident) {
                    Some(p) => p,
                    None => {
                        return Err(parser.new_custom_error(ParseErrorOwned(format!(
                            "unknown pseudo-class :{ident}"
                        ))));
                    }
                };
                if sel.pseudo.is_some() {
                    return Err(parser.new_custom_error(ParseErrorOwned(
                        "multiple pseudo-classes per selector are not supported".to_string(),
                    )));
                }
                sel.pseudo = Some(pseudo);
                saw_anything = true;
            }
            _ => {
                parser.reset(&save);
                break;
            }
        }
    }
    if !saw_anything {
        return Err(parser.new_custom_error(ParseErrorOwned("empty selector".to_string())));
    }
    Ok(sel)
}

/// Parse a compound selector: one or more [`SimpleSelector`]s joined by
/// [`Combinator`]s. Handles ` ` (descendant), `>` (child), `+`
/// (adjacent sibling), `~` (general sibling).
fn parse_compound_selector<'i, 't>(
    parser: &mut Parser<'i, 't>,
) -> Result<Selector, CssParseError<'i, ParseErrorOwned>> {
    // Skip leading whitespace before the selector starts — cssparser keeps
    // the whitespace token between e.g. `,` and the next selector.
    parser.skip_whitespace();
    let first = parse_simple_selector(parser, true)?;
    let mut parts = vec![first];
    let mut combinators = vec![];

    loop {
        // Peek at what follows: whitespace, explicit combinator token, or
        // something that cannot continue a selector.
        let save = parser.state();
        let tok = parser.next_including_whitespace().cloned();

        match tok {
            // Explicit combinators: `>`, `+`, `~` (possibly with surrounding
            // whitespace already consumed in the whitespace arm below).
            Ok(Token::Delim('>')) => {
                parser.skip_whitespace();
                let next_simple = parse_simple_selector(parser, true)?;
                parts.push(next_simple);
                combinators.push(Combinator::Child);
            }
            Ok(Token::Delim('+')) => {
                parser.skip_whitespace();
                let next_simple = parse_simple_selector(parser, true)?;
                parts.push(next_simple);
                combinators.push(Combinator::AdjacentSibling);
            }
            Ok(Token::Delim('~')) => {
                parser.skip_whitespace();
                let next_simple = parse_simple_selector(parser, true)?;
                parts.push(next_simple);
                combinators.push(Combinator::GeneralSibling);
            }
            Ok(Token::WhiteSpace(_)) => {
                // Could be a descendant combinator or just trailing
                // whitespace before `{`. Peek further.
                //
                // Eat any *additional* whitespace tokens. cssparser emits
                // one Token::WhiteSpace per contiguous whitespace run, but
                // a CSS comment between two runs (e.g. `.a /* x */ .b`)
                // separates them into two tokens. Without this skip the
                // second whitespace falls into the `_` arm of the inner
                // match, the loop breaks, and the trailing `.b` is left
                // unconsumed — cssparser then drops the whole rule.
                parser.skip_whitespace();
                let after_ws = parser.state();
                let next_tok = parser.next_including_whitespace().cloned();
                match next_tok {
                    // Explicit combinator after whitespace: ` > .b`, ` + .b`, ` ~ .b`.
                    Ok(Token::Delim('>')) => {
                        parser.skip_whitespace();
                        let next_simple = parse_simple_selector(parser, true)?;
                        parts.push(next_simple);
                        combinators.push(Combinator::Child);
                    }
                    Ok(Token::Delim('+')) => {
                        parser.skip_whitespace();
                        let next_simple = parse_simple_selector(parser, true)?;
                        parts.push(next_simple);
                        combinators.push(Combinator::AdjacentSibling);
                    }
                    Ok(Token::Delim('~')) => {
                        parser.skip_whitespace();
                        let next_simple = parse_simple_selector(parser, true)?;
                        parts.push(next_simple);
                        combinators.push(Combinator::GeneralSibling);
                    }
                    // Tokens that can start a simple selector → descendant combinator.
                    // Put the token back and re-parse — the Ident case needs
                    // allow_type=true so `label span` works.
                    Ok(Token::Ident(_)) | Ok(Token::Delim('.')) | Ok(Token::Colon) => {
                        parser.reset(&after_ws);
                        let next_simple = parse_simple_selector(parser, true)?;
                        parts.push(next_simple);
                        combinators.push(Combinator::Descendant);
                    }
                    _ => {
                        // Trailing whitespace before `{` or end — not a
                        // combinator. Back up past the whitespace too.
                        parser.reset(&save);
                        break;
                    }
                }
            }
            _ => {
                parser.reset(&save);
                break;
            }
        }
    }

    Ok(Selector { parts, combinators })
}

struct DeclParser;

impl<'i> DeclarationParser<'i> for DeclParser {
    type Declaration = Declaration;
    type Error = ParseErrorOwned;

    fn parse_value<'t>(
        &mut self,
        name: CowRcStr<'i>,
        parser: &mut Parser<'i, 't>,
        _start: &ParserState,
    ) -> Result<Self::Declaration, CssParseError<'i, Self::Error>> {
        let prop = name.to_string();
        let (value, important) = parse_value(&prop, parser)?;
        Ok(Declaration {
            property: prop,
            value,
            important,
        })
    }
}

impl<'i> AtRuleParser<'i> for DeclParser {
    type Prelude = ();
    type AtRule = Declaration;
    type Error = ParseErrorOwned;
}

impl<'i> QualifiedRuleParser<'i> for DeclParser {
    type Prelude = ();
    type QualifiedRule = Declaration;
    type Error = ParseErrorOwned;
}

impl<'i> RuleBodyItemParser<'i, Declaration, ParseErrorOwned> for DeclParser {
    fn parse_declarations(&self) -> bool {
        true
    }
    fn parse_qualified(&self) -> bool {
        false
    }
}

fn parse_value<'i, 't>(
    prop: &str,
    parser: &mut Parser<'i, 't>,
) -> Result<(Value, bool), CssParseError<'i, ParseErrorOwned>> {
    let value = parse_value_inner(prop, parser)?;
    let important = consume_important(parser);
    parser.expect_exhausted()?;
    Ok((value, important))
}

fn parse_value_inner<'i, 't>(
    prop: &str,
    parser: &mut Parser<'i, 't>,
) -> Result<Value, CssParseError<'i, ParseErrorOwned>> {
    // Per-property value type: each recognised property accepts only the
    // value shape it actually means. This stops `color: nonsense` from
    // silently parsing as a keyword and reaching the adapter.
    match property_kind(prop) {
        PropertyKind::Color => parse_color(parser).map(Value::Color),
        PropertyKind::Length => parse_length(parser).map(Value::Length),
        PropertyKind::LengthOrAuto => {
            if parser.try_parse(expect_auto).is_ok() {
                Ok(Value::Auto)
            } else {
                parse_length(parser).map(Value::Length)
            }
        }
        PropertyKind::SideLengths => {
            let mut lengths = Vec::new();
            while let Ok(len) = parser.try_parse(parse_length) {
                lengths.push(len);
                if lengths.len() == 4 {
                    break;
                }
            }
            if lengths.is_empty() {
                return Err(parser
                    .new_custom_error(ParseErrorOwned(format!("expected length for `{prop}`"))));
            }
            Ok(Value::LengthSet(lengths))
        }
        PropertyKind::SideLengthsOrAuto => parse_side_lengths_or_auto(prop, parser),
        PropertyKind::Keyword(allowed) => {
            let ident = parser.try_parse(|p| p.expect_ident_cloned()).map_err(|_| {
                parser.new_custom_error(ParseErrorOwned(format!("expected keyword for `{prop}`")))
            })?;
            let kw = ident.to_ascii_lowercase();
            if allowed.contains(&kw.as_str()) {
                Ok(Value::Keyword(kw))
            } else {
                Err(parser.new_custom_error(ParseErrorOwned(format!(
                    "unknown keyword `{kw}` for `{prop}`"
                ))))
            }
        }
        PropertyKind::Number => {
            let n = parser.try_parse(|p| p.expect_number()).map_err(|_| {
                parser.new_custom_error(ParseErrorOwned(format!("expected number for `{prop}`")))
            })?;
            // `flex-grow` / `flex-shrink` are spec-required to be >= 0;
            // every property using `PropertyKind::Number` today inherits
            // that constraint. If a future property needs signed numbers,
            // split into a separate `SignedNumber` kind.
            if n < 0.0 {
                return Err(parser.new_custom_error(ParseErrorOwned(format!(
                    "negative number not allowed for `{prop}`"
                ))));
            }
            Ok(Value::Number(f64::from(n)))
        }
        PropertyKind::NumberOrLength => {
            // Try unitless number first (line-height: 1.5), then length.
            // A dimension token like `24px` is NOT a plain Number in
            // cssparser, so `expect_number` won't consume it — try in order.
            if let Ok(n) = parser.try_parse(|p| {
                let loc = p.current_source_location();
                let tok = p.next()?.clone();
                match tok {
                    // Accept only a pure Number token (no unit).
                    Token::Number { value, .. } => Ok(value),
                    other => Err(loc.new_custom_error::<ParseErrorOwned, ParseErrorOwned>(
                        ParseErrorOwned(format!("not a plain number: {other:?}")),
                    )),
                }
            }) {
                Ok(Value::Number(f64::from(n)))
            } else {
                parse_length(parser).map(Value::Length)
            }
        }
        PropertyKind::FontWeight => {
            if let Ok(n) = parser.try_parse(|p| p.expect_number()) {
                let n = f64::from(n);
                // CSS spec: font-weight numeric values must be integers in 1..=1000.
                if n.fract() != 0.0 || !(1.0..=1000.0).contains(&n) {
                    return Err(parser.new_custom_error(ParseErrorOwned(format!(
                        "font-weight numeric value `{n}` is out of range (must be integer 1–1000)"
                    ))));
                }
                Ok(Value::Number(n))
            } else {
                let ident = parser.try_parse(|p| p.expect_ident_cloned()).map_err(|_| {
                    parser.new_custom_error(ParseErrorOwned(
                        "expected number or keyword for `font-weight`".to_string(),
                    ))
                })?;
                let kw = ident.to_ascii_lowercase();
                if ["normal", "bold"].contains(&kw.as_str()) {
                    Ok(Value::Keyword(kw))
                } else {
                    Err(parser.new_custom_error(ParseErrorOwned(format!(
                        "unknown keyword `{kw}` for `font-weight`"
                    ))))
                }
            }
        }
        PropertyKind::FontFamily => parse_font_family(parser),
        PropertyKind::Border => parse_border_shorthand(prop, parser),
        PropertyKind::Unknown => {
            // Forward-compat: unknown properties (anything we'll grow into
            // later) take whichever shape the value tokens fit. Try color,
            // then length, then bare keyword.
            if let Ok(c) = parser.try_parse(parse_color) {
                return Ok(Value::Color(c));
            }
            if let Ok(len) = parser.try_parse(parse_length) {
                return Ok(Value::Length(len));
            }
            if let Ok(ident) = parser.try_parse(|p| p.expect_ident_cloned()) {
                // Match the lowercasing the strict keyword arm applies, so
                // unknown-property keyword values cascade case-insensitively
                // with each other.
                return Ok(Value::Keyword(ident.to_ascii_lowercase()));
            }
            Err(parser.new_custom_error(ParseErrorOwned(format!(
                "could not parse value for `{prop}`"
            ))))
        }
    }
}

// -- side-lengths-or-auto ----------------------------------------------------

fn parse_side_lengths_or_auto<'i, 't>(
    prop: &str,
    parser: &mut Parser<'i, 't>,
) -> Result<Value, CssParseError<'i, ParseErrorOwned>> {
    let mut sides: Vec<SideValue> = Vec::new();
    loop {
        if sides.len() == 4 {
            break;
        }
        if let Ok(()) = parser.try_parse(expect_auto) {
            sides.push(SideValue::Auto);
        } else if let Ok(len) = parser.try_parse(parse_length) {
            sides.push(SideValue::Length(len));
        } else {
            break;
        }
    }
    if sides.is_empty() {
        return Err(parser.new_custom_error(ParseErrorOwned(format!(
            "expected length or auto for `{prop}`"
        ))));
    }
    // Single `auto` token → Value::Auto (matches `width: auto` semantics).
    if sides == [SideValue::Auto] {
        return Ok(Value::Auto);
    }
    // If every side is a plain length, downcast to LengthSet so consumers
    // that only handle LengthSet still work.
    let all_lengths: Option<Vec<Length>> = sides
        .iter()
        .map(|sv| match sv {
            SideValue::Length(l) => Some(*l),
            SideValue::Auto => None,
        })
        .collect();
    if let Some(lengths) = all_lengths {
        return Ok(Value::LengthSet(lengths));
    }
    Ok(Value::SideSet(sides))
}

// -- font-family -------------------------------------------------------------

fn parse_font_family<'i, 't>(
    parser: &mut Parser<'i, 't>,
) -> Result<Value, CssParseError<'i, ParseErrorOwned>> {
    let mut families: Vec<String> = Vec::new();
    let mut pending_comma = false;
    loop {
        // Accept a quoted string or one or more unquoted idents.
        let pushed = if let Ok(s) = parser.try_parse(|p| p.expect_string_cloned()) {
            families.push(s.to_string());
            true
        } else if let Ok(ident) = parser.try_parse(|p| p.expect_ident_cloned()) {
            // Concatenate adjacent idents for multi-word names like `sans serif`.
            // CSS spec: unquoted family name = sequence of idents.
            let mut name = ident.to_string();
            loop {
                // peek: if next non-whitespace token is an ident (and no comma
                // or EOF), keep appending.
                let state = parser.state();
                match parser.next_including_whitespace() {
                    Ok(Token::WhiteSpace(_)) => {
                        let state2 = parser.state();
                        match parser.next_including_whitespace() {
                            Ok(Token::Ident(next_ident)) => {
                                name.push(' ');
                                name.push_str(next_ident.as_ref());
                            }
                            _ => {
                                parser.reset(&state2);
                                break;
                            }
                        }
                    }
                    _ => {
                        parser.reset(&state);
                        break;
                    }
                }
            }
            families.push(name);
            true
        } else {
            false
        };
        if !pushed {
            if pending_comma {
                // `font-family: "Hack",` — comma not followed by another
                // family name. Reject so the malformed declaration is
                // dropped.
                return Err(parser
                    .new_custom_error(ParseErrorOwned("trailing comma in font-family".into())));
            }
            break;
        }
        if parser.try_parse(|p| p.expect_comma()).is_err() {
            break;
        }
        pending_comma = true;
    }
    if families.is_empty() {
        return Err(parser.new_custom_error(ParseErrorOwned("expected font-family value".into())));
    }
    Ok(Value::FontFamilyList(families))
}

// -- border shorthand --------------------------------------------------------

fn parse_border_shorthand<'i, 't>(
    prop: &str,
    parser: &mut Parser<'i, 't>,
) -> Result<Value, CssParseError<'i, ParseErrorOwned>> {
    // Accept `<length> [solid|none] <color>` in any order.
    // `style` token if present must be `solid` or `none`; others reject.
    // All three are required (width + color mandatory; style optional).
    let mut width: Option<Length> = None;
    let mut color: Option<Color> = None;
    let mut saw_none_style = false;

    // Try each token up to 3 times (at most: length, style, color).
    for _ in 0..3 {
        if width.is_none()
            && let Ok(len) = parser.try_parse(parse_length)
        {
            width = Some(len);
            continue;
        }
        if color.is_none()
            && let Ok(c) = parser.try_parse(parse_color)
        {
            color = Some(c);
            continue;
        }
        // style keyword: solid (accepted, ignored) or none (zero width).
        if let Ok(ident) = parser.try_parse(|p| p.expect_ident_cloned()) {
            let kw = ident.to_ascii_lowercase();
            match kw.as_str() {
                // `none` is special: it zeros the width when no width was
                // given and makes the color optional.
                "none" => {
                    saw_none_style = true;
                    continue;
                }
                // Every other style keyword (`solid`, `dashed`, `dotted`,
                // `double`, `groove`, `ridge`, `inset`, `outset`, …) is
                // accepted and ignored — floem has no border-style model,
                // so we don't promote the choice into the AST. Erroring
                // would drop the whole declaration including the width
                // and color the user actually cares about.
                _ => continue,
            }
        }
        break;
    }

    let width = if saw_none_style {
        // `none` style: width is optional. If the source gave an explicit
        // width (e.g. `border: 1px none red`) keep it — the user is
        // describing a transition-style hidden border. If they omitted the
        // width (`border: none red` / `border: none`) treat the border as
        // zero-thickness.
        width.unwrap_or(Length::Px(0.0))
    } else {
        width.ok_or_else(|| {
            parser.new_custom_error(ParseErrorOwned(format!("missing width in `{prop}`")))
        })?
    };
    // `border: none` (no color) is the most common CSS reset. Allow it
    // when the user wrote `none`: fall back to transparent so the border
    // is structurally present in the AST but visually invisible.
    let color = match color {
        Some(c) => c,
        None if saw_none_style => Color::rgba(0, 0, 0, 0),
        None => {
            return Err(
                parser.new_custom_error(ParseErrorOwned(format!("missing color in `{prop}`")))
            );
        }
    };

    Ok(Value::Border { width, color })
}

// -- helpers -----------------------------------------------------------------

fn expect_auto<'i, 't>(
    parser: &mut Parser<'i, 't>,
) -> Result<(), CssParseError<'i, ParseErrorOwned>> {
    let loc = parser.current_source_location();
    let tok = parser.next()?.clone();
    match &tok {
        Token::Ident(name) if name.eq_ignore_ascii_case("auto") => Ok(()),
        other => {
            Err(loc.new_custom_error(ParseErrorOwned(format!("expected `auto`, got {other:?}"))))
        }
    }
}

// -- property kind -----------------------------------------------------------

#[derive(Debug, Clone)]
enum PropertyKind {
    Color,
    Length,
    /// `width`, `height`, `flex-basis`: length OR `auto`.
    LengthOrAuto,
    /// `padding`, `border-radius`: 1..=4 lengths only.
    SideLengths,
    /// `margin`: 1..=4 sides, each length or auto.
    SideLengthsOrAuto,
    /// Fixed set of keyword values.
    Keyword(&'static [&'static str]),
    /// Unitless number only.
    Number,
    /// Unitless number (yields `Value::Number`) OR length (yields `Value::Length`).
    NumberOrLength,
    FontFamily,
    /// `font-weight`: integer in 1..=1000 OR keyword `normal`/`bold`.
    FontWeight,
    Border,
    /// Forward-compat shape for properties not yet first-class.
    Unknown,
}

fn property_kind(name: &str) -> PropertyKind {
    match name {
        "color" | "background-color" => PropertyKind::Color,

        // Sizing
        "width" | "height" | "flex-basis" => PropertyKind::LengthOrAuto,

        // Box spacing
        "padding" | "border-radius" => PropertyKind::SideLengths,
        "margin" => PropertyKind::SideLengthsOrAuto,
        "gap" | "row-gap" | "column-gap" => PropertyKind::Length,

        // Layout
        "display" => PropertyKind::Keyword(&["flex", "block", "none"]),
        "flex-direction" => {
            PropertyKind::Keyword(&["row", "column", "row-reverse", "column-reverse"])
        }
        "align-items" => PropertyKind::Keyword(&["start", "end", "center", "stretch", "baseline"]),
        "justify-content" => PropertyKind::Keyword(&[
            "start",
            "end",
            "center",
            "space-between",
            "space-around",
            "space-evenly",
        ]),
        "flex-grow" | "flex-shrink" => PropertyKind::Number,

        // Border shorthands
        "border" | "border-top" | "border-right" | "border-bottom" | "border-left" | "outline" => {
            PropertyKind::Border
        }

        // CSS spec: `border-width` is a 1..=4 length shorthand
        // (top/right/bottom/left), same expansion rules as padding.
        "border-width" => PropertyKind::SideLengths,
        "border-color" => PropertyKind::Color,
        "border-top-color" | "border-right-color" | "border-bottom-color" | "border-left-color" => {
            PropertyKind::Color
        }

        // Typography
        "font-family" => PropertyKind::FontFamily,
        "font-size" => PropertyKind::Length,
        "font-weight" => PropertyKind::FontWeight,
        "font-style" => PropertyKind::Keyword(&["normal", "italic", "oblique"]),
        "text-align" => PropertyKind::Keyword(&["left", "center", "right", "justify"]),
        "line-height" => PropertyKind::NumberOrLength,

        _ => PropertyKind::Unknown,
    }
}

/// Consume a trailing `!important` if present. The flag is surfaced on
/// the resulting [`Declaration`] and honoured by the cascade in
/// [`crate::Stylesheet::resolve`] — important declarations beat any
/// non-important declaration regardless of specificity, with source
/// order breaking ties within either tier.
fn consume_important(parser: &mut Parser<'_, '_>) -> bool {
    parser
        .try_parse(|p| -> Result<(), CssParseError<'_, ParseErrorOwned>> {
            p.expect_delim('!')?;
            let ident = p.expect_ident_cloned()?;
            if ident.eq_ignore_ascii_case("important") {
                Ok(())
            } else {
                Err(p.new_custom_error(ParseErrorOwned("not !important".to_string())))
            }
        })
        .is_ok()
}

fn parse_length<'i, 't>(
    parser: &mut Parser<'i, 't>,
) -> Result<Length, CssParseError<'i, ParseErrorOwned>> {
    let location = parser.current_source_location();
    let token = parser.next()?.clone();
    match token {
        Token::Dimension { value, unit, .. } => match unit.as_ref() {
            "px" => Ok(Length::Px(f64::from(value))),
            other => Err(location.new_custom_error(ParseErrorOwned(format!(
                // `em` / `rem` deferred to a later phase.
                "unsupported length unit `{other}`"
            )))),
        },
        Token::Percentage { unit_value, .. } => Ok(Length::Percent(f64::from(unit_value) * 100.0)),
        Token::Number { value, .. } => Ok(Length::Px(f64::from(value))),
        other => {
            Err(location
                .new_custom_error(ParseErrorOwned(format!("expected length, got {other:?}"))))
        }
    }
}

fn parse_color<'i, 't>(
    parser: &mut Parser<'i, 't>,
) -> Result<Color, CssParseError<'i, ParseErrorOwned>> {
    let location = parser.current_source_location();
    let token = parser.next()?.clone();
    match token {
        // cssparser emits `Hash` when the value starts with a digit
        // (e.g. `#1a2b3c`) and `IDHash` otherwise — both are valid CSS
        // colour syntax, so collapse them into a single arm.
        Token::IDHash(h) | Token::Hash(h) => parse_hex(h.as_ref()).ok_or_else(|| {
            location.new_custom_error(ParseErrorOwned(format!("bad hex color `#{h}`")))
        }),
        Token::Ident(name) => named_color(name.as_ref()).ok_or_else(|| {
            location.new_custom_error(ParseErrorOwned(format!("unknown color name `{name}`")))
        }),
        Token::Function(name) => {
            let name_lc = name.to_ascii_lowercase();
            parser.parse_nested_block(|p| match name_lc.as_str() {
                "rgb" => parse_rgb_args(p, false),
                "rgba" => parse_rgb_args(p, true),
                other => Err(p.new_custom_error(ParseErrorOwned(format!(
                    "unsupported color function `{other}`"
                )))),
            })
        }
        other => {
            Err(location
                .new_custom_error(ParseErrorOwned(format!("expected color, got {other:?}"))))
        }
    }
}

fn parse_rgb_args<'i, 't>(
    parser: &mut Parser<'i, 't>,
    expect_alpha: bool,
) -> Result<Color, CssParseError<'i, ParseErrorOwned>> {
    let r = parse_u8_channel(parser)?;
    parser.expect_comma()?;
    let g = parse_u8_channel(parser)?;
    parser.expect_comma()?;
    let b = parse_u8_channel(parser)?;
    let a = if expect_alpha {
        parser.expect_comma()?;
        let f = parser.expect_number()?;
        (f.clamp(0.0, 1.0) * 255.0).round() as u8
    } else {
        0xff
    };
    parser.expect_exhausted()?;
    Ok(Color::rgba(r, g, b, a))
}

fn parse_u8_channel<'i, 't>(
    parser: &mut Parser<'i, 't>,
) -> Result<u8, CssParseError<'i, ParseErrorOwned>> {
    let location = parser.current_source_location();
    let token = parser.next()?.clone();
    let n = match token {
        Token::Number { value, .. } => value,
        Token::Percentage { unit_value, .. } => unit_value * 255.0,
        other => {
            return Err(location
                .new_custom_error(ParseErrorOwned(format!("expected channel, got {other:?}"))));
        }
    };
    Ok(n.clamp(0.0, 255.0).round() as u8)
}

fn parse_hex(s: &str) -> Option<Color> {
    let hex = |c: char| c.to_digit(16).map(|d| d as u8);
    let chars: Vec<u8> = s.chars().filter_map(hex).collect();
    if chars.len() != s.chars().count() {
        return None;
    }
    let dup = |n: u8| (n << 4) | n;
    Some(match chars.len() {
        3 => Color::rgb(dup(chars[0]), dup(chars[1]), dup(chars[2])),
        4 => Color::rgba(dup(chars[0]), dup(chars[1]), dup(chars[2]), dup(chars[3])),
        6 => Color::rgb(
            (chars[0] << 4) | chars[1],
            (chars[2] << 4) | chars[3],
            (chars[4] << 4) | chars[5],
        ),
        8 => Color::rgba(
            (chars[0] << 4) | chars[1],
            (chars[2] << 4) | chars[3],
            (chars[4] << 4) | chars[5],
            (chars[6] << 4) | chars[7],
        ),
        _ => return None,
    })
}

fn named_color(name: &str) -> Option<Color> {
    // CSS Color Module 4 canonical hex values.
    Some(match_ignore_ascii_case! { name,
        "transparent" => Color::rgba(0, 0, 0, 0),
        // CSS Level 1 (16 colors)
        "black"   => Color::rgb(0x00, 0x00, 0x00),
        "silver"  => Color::rgb(0xc0, 0xc0, 0xc0),
        "gray"    => Color::rgb(0x80, 0x80, 0x80),
        "grey"    => Color::rgb(0x80, 0x80, 0x80),
        "white"   => Color::rgb(0xff, 0xff, 0xff),
        "maroon"  => Color::rgb(0x80, 0x00, 0x00),
        "red"     => Color::rgb(0xff, 0x00, 0x00),
        "purple"  => Color::rgb(0x80, 0x00, 0x80),
        "fuchsia" => Color::rgb(0xff, 0x00, 0xff),
        "green"   => Color::rgb(0x00, 0x80, 0x00),
        "lime"    => Color::rgb(0x00, 0xff, 0x00),
        "olive"   => Color::rgb(0x80, 0x80, 0x00),
        "yellow"  => Color::rgb(0xff, 0xff, 0x00),
        "navy"    => Color::rgb(0x00, 0x00, 0x80),
        "blue"    => Color::rgb(0x00, 0x00, 0xff),
        "teal"    => Color::rgb(0x00, 0x80, 0x80),
        "aqua"    => Color::rgb(0x00, 0xff, 0xff),
        // Common aliases / extras
        "cyan"    => Color::rgb(0x00, 0xff, 0xff),
        "magenta" => Color::rgb(0xff, 0x00, 0xff),
        "orange"  => Color::rgb(0xff, 0xa5, 0x00),
        "brown"   => Color::rgb(0xa5, 0x2a, 0x2a),
        "pink"    => Color::rgb(0xff, 0xc0, 0xcb),
        _ => return None,
    })
}
