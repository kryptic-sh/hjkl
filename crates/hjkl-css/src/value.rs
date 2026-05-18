//! Property values. v1 covers the handful needed by phase-1 properties
//! (background-color, color, padding, margin, width, height); phase-2
//! adds Auto, Number, FontFamilyList, Border, and SideSet for mixed
//! length/auto shorthands.

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Color(Color),
    Length(Length),
    /// 1..=4 lengths, as in CSS shorthand: `padding: 1px` /
    /// `padding: 1px 2px` / `padding: 1px 2px 3px` /
    /// `padding: 1px 2px 3px 4px`.
    LengthSet(Vec<Length>),
    Keyword(String),
    /// `auto` keyword used with sizing and margin properties.
    Auto,
    /// Unitless number — `flex-grow`, `flex-shrink`, `line-height`,
    /// `font-weight` (numeric form).
    Number(f64),
    /// `font-family` list: one or more family names in source order.
    /// Quoted strings and bare idents both land here as plain strings.
    FontFamilyList(Vec<String>),
    /// `border` / `border-{side}` / `outline` shorthand.
    /// `style` is dropped — floem has no border-style model; `border: 1px
    /// none #fff` treats `none` as zero width (same as omitting a
    /// visible line). The `solid` token, if present, is accepted and
    /// ignored.
    Border {
        width: Length,
        color: Color,
    },
    /// 1..=4 side values where each side may be a length or `auto`.
    /// Used for `margin` / `padding` shorthands that contain `auto`.
    ///
    /// Trade-off: a separate `SideSet` variant rather than making `Length`
    /// an `Option<Length>` or adding `Length::Auto`. Keeping `Length` as
    /// a pure numeric type avoids propagating `auto`-awareness into every
    /// length consumer; adapters that only care about `LengthSet` stay
    /// unchanged.
    SideSet(Vec<SideValue>),
}

/// One side in a mixed length/auto shorthand (`margin: 4px auto`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SideValue {
    Length(Length),
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

/// `Length` carries `f64` payloads so it intentionally only implements
/// `PartialEq` — `f64` is not `Eq`. By extension, [`Value::LengthSet`]
/// and any container of [`Value`] cannot be `Eq` either. Adapters that
/// want hashable/`Eq`-equivalent comparison should compare a derived
/// representation (e.g. rendered floem `Style`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Length {
    /// CSS pixels. Unitless values in the source are parsed as Px too —
    /// floem doesn't distinguish, and the layout engine treats both the
    /// same.
    ///
    /// `em` / `rem` are deferred to a later phase; document the gap here
    /// so it is easy to find.
    Px(f64),
    /// `<n>%` of the parent's relevant dimension.
    Percent(f64),
}

impl Color {
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self::rgba(r, g, b, 0xff)
    }
}

impl Length {
    pub fn as_px(self) -> Option<f64> {
        match self {
            Self::Px(v) => Some(v),
            Self::Percent(_) => None,
        }
    }
}

/// Expand a 1..=4 length shorthand to the four sides (top, right, bottom,
/// left). Mirrors CSS's `padding` / `margin` rules.
///
/// Returns `None` for empty or 5+-length input. The parser only emits
/// `LengthSet`s in the 1..=4 range, so adapters consuming `Value::LengthSet`
/// values that came from `parse` can `.unwrap()` safely. Adapter code
/// constructing a `LengthSet` programmatically should guard against the
/// out-of-range case.
pub fn expand_sides(set: &[Length]) -> Option<[Length; 4]> {
    match set.len() {
        1 => Some([set[0]; 4]),
        2 => Some([set[0], set[1], set[0], set[1]]),
        3 => Some([set[0], set[1], set[2], set[1]]),
        4 => Some([set[0], set[1], set[2], set[3]]),
        _ => None,
    }
}

/// Expand a 1..=4 `SideValue` shorthand to four sides.
pub fn expand_side_set(set: &[SideValue]) -> Option<[SideValue; 4]> {
    match set.len() {
        1 => Some([set[0]; 4]),
        2 => Some([set[0], set[1], set[0], set[1]]),
        3 => Some([set[0], set[1], set[2], set[1]]),
        4 => Some([set[0], set[1], set[2], set[3]]),
        _ => None,
    }
}
