//! Computed value types.
//!
//! *Computed*, not *specified*: font-relative units are already resolved to pixels by the
//! time a value lands in one of these. Percentages are not, because a percentage resolves
//! against a containing block that layout has not measured yet — `taffy` takes them
//! unresolved and does the arithmetic itself.
//!
//! Every type here is `Eq + Hash`, because [`crate::ComputedStyle`] is a hash map key: two
//! nodes with identical computed style must share one allocation (ROADMAP §M3), and that
//! means style has to be hashable. Floats are compared and hashed by bit pattern, which is
//! a true equivalence relation as long as no NaN gets in — see [`Px::new`].

use std::hash::{Hash, Hasher};

/// A length in pixels.
///
/// A newtype rather than a bare `f32` so that [`Eq`] and [`Hash`] can be defined on it once
/// instead of on every struct that contains a length.
#[derive(Clone, Copy, Debug, Default, PartialOrd)]
pub struct Px(f32);

/// A unitless number: `opacity`, `flex-grow`, a unitless `line-height`.
#[derive(Clone, Copy, Debug, Default, PartialOrd)]
pub struct Number(f32);

macro_rules! float_newtype {
    ($name:ident, $doc:literal) => {
        impl $name {
            #[doc = $doc]
            ///
            /// A non-finite input becomes zero. `calc()` can produce one — `calc(1px / 0)` —
            /// and a NaN reaching layout is both a wrong box and a broken `Eq`, since NaN is
            /// not equal to itself and the style interner is a hash map keyed on this.
            ///
            /// Negative zero is normalised to positive zero. Equality here is bitwise, and
            /// `-0.0` and `0.0` have different bit patterns, so leaving both in would make
            /// two identical styles hash differently — halving the interner's hit rate on
            /// any style that reaches zero by subtraction.
            #[must_use]
            pub fn new(value: f32) -> Self {
                Self(if value.is_finite() { value + 0.0 } else { 0.0 })
            }

            /// Zero.
            pub const ZERO: Self = Self(0.0);

            /// The value.
            #[must_use]
            pub fn get(self) -> f32 {
                self.0
            }
        }

        impl PartialEq for $name {
            fn eq(&self, other: &Self) -> bool {
                self.0.to_bits() == other.0.to_bits()
            }
        }

        impl Eq for $name {}

        impl Hash for $name {
            fn hash<H: Hasher>(&self, state: &mut H) {
                self.0.to_bits().hash(state);
            }
        }

        impl From<f32> for $name {
            fn from(value: f32) -> Self {
                Self::new(value)
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

float_newtype!(Px, "A length in pixels.");
float_newtype!(Number, "A unitless number.");

impl Px {
    /// One CSS pixel.
    pub const ONE: Self = Self(1.0);
}

impl Number {
    /// One.
    pub const ONE: Self = Self(1.0);

    /// Clamped to `0.0..=1.0`, for `opacity`.
    #[must_use]
    pub fn clamped_unit(self) -> Self {
        Self(self.0.clamp(0.0, 1.0))
    }
}

/// A length or a percentage. Percentages stay unresolved for layout to handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LengthPercentage {
    /// An absolute length.
    Px(Px),
    /// A fraction of the containing block, as written: `50%` is `Percent(50.0)`.
    Percent(Number),
}

impl Default for LengthPercentage {
    fn default() -> Self {
        Self::Px(Px::ZERO)
    }
}

impl LengthPercentage {
    /// Zero pixels.
    pub const ZERO: Self = Self::Px(Px::ZERO);

    /// Resolves against a containing-block extent.
    ///
    /// Layout normally hands percentages to `taffy` unresolved; this is for the places that
    /// cannot, such as a `border-radius` that has to be a number of pixels before it can be
    /// painted.
    #[must_use]
    pub fn resolve(self, basis: f32) -> f32 {
        match self {
            Self::Px(px) => px.get(),
            Self::Percent(fraction) => fraction.get() / 100.0 * basis,
        }
    }
}

/// A size that may also be `auto`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Dimension {
    /// Decided by layout.
    #[default]
    Auto,
    /// A length or percentage.
    Length(LengthPercentage),
}

impl Dimension {
    /// A dimension in pixels.
    #[must_use]
    pub fn px(value: f32) -> Self {
        Self::Length(LengthPercentage::Px(Px::new(value)))
    }

    /// A percentage, as written: `50%` is `percent(50.0)`.
    #[must_use]
    pub fn percent(value: f32) -> Self {
        Self::Length(LengthPercentage::Percent(Number::new(value)))
    }
}

/// A value per box edge, in CSS order.
///
/// Generic because `margin`, `padding`, `border-width`, `border-color` and `inset` all have
/// this shape and all need the same per-edge resolution from shorthands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Sides<T> {
    /// Top edge.
    pub top: T,
    /// Right edge.
    pub right: T,
    /// Bottom edge.
    pub bottom: T,
    /// Left edge.
    pub left: T,
}

impl<T: Copy> Sides<T> {
    /// The same value on all four edges.
    pub const fn all(value: T) -> Self {
        Self {
            top: value,
            right: value,
            bottom: value,
            left: value,
        }
    }

    /// Applies `f` to each edge.
    pub fn map<U>(self, mut f: impl FnMut(T) -> U) -> Sides<U> {
        Sides {
            top: f(self.top),
            right: f(self.right),
            bottom: f(self.bottom),
            left: f(self.left),
        }
    }
}

/// A radius per corner, in CSS order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct CornerRadii {
    /// Top-left.
    pub top_left: LengthPercentage,
    /// Top-right.
    pub top_right: LengthPercentage,
    /// Bottom-right.
    pub bottom_right: LengthPercentage,
    /// Bottom-left.
    pub bottom_left: LengthPercentage,
}

/// An sRGB colour, comparable and hashable.
///
/// A separate type from [`crisol_display_list::Color`] only because that one holds `f32`
/// and therefore cannot be a hash map key. They convert both ways and mean the same thing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Color {
    /// Red, sRGB-encoded.
    pub r: u8,
    /// Green, sRGB-encoded.
    pub g: u8,
    /// Blue, sRGB-encoded.
    pub b: u8,
    /// Alpha.
    pub a: u8,
}

impl Color {
    /// Fully transparent.
    pub const TRANSPARENT: Self = Self::rgba(0, 0, 0, 0);
    /// Opaque black.
    pub const BLACK: Self = Self::rgba(0, 0, 0, 255);
    /// Opaque white.
    pub const WHITE: Self = Self::rgba(255, 255, 255, 255);

    /// A colour from 8-bit sRGB components.
    #[must_use]
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// True when this colour cannot change any pixel.
    #[must_use]
    pub fn is_transparent(self) -> bool {
        self.a == 0
    }
}

impl From<Color> for crisol_display_list::Color {
    fn from(color: Color) -> Self {
        Self::from_rgba8(color.r, color.g, color.b, color.a)
    }
}

/// How a box participates in layout.
///
/// A short list. `inline` is absent because inline layout is M4's problem; `grid` is absent
/// because none of the `grid-template-*` properties are in M3's subset, so a grid would lay
/// out as a single column — worse than declining the declaration. ROADMAP §1 rules out
/// tables-as-layout and floats outright. In every case an unsupported `display` leaves the
/// property at its previous value rather than quietly laying the box out as something else.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Display {
    /// No box at all. The subtree is not laid out and not painted.
    None,
    /// Block layout.
    #[default]
    Block,
    /// Flex layout.
    Flex,
}

/// How a box is positioned relative to its normal place.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Position {
    /// In the normal flow. `inset` still shifts it visually without affecting siblings.
    #[default]
    Relative,
    /// Out of flow, positioned against the nearest positioned ancestor.
    Absolute,
}

/// Main-axis direction for flex layout.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum FlexDirection {
    /// Left to right.
    #[default]
    Row,
    /// Right to left.
    RowReverse,
    /// Top to bottom.
    Column,
    /// Bottom to top.
    ColumnReverse,
}

/// Whether flex items wrap.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum FlexWrap {
    /// One line, items shrink.
    #[default]
    NoWrap,
    /// Multiple lines.
    Wrap,
    /// Multiple lines, in reverse cross-axis order.
    WrapReverse,
}

/// Main-axis distribution.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum JustifyContent {
    /// Packed at the start.
    Start,
    /// Packed at the end.
    End,
    /// Packed at the centre.
    Center,
    /// First and last flush, equal gaps between.
    SpaceBetween,
    /// Equal space around each item.
    SpaceAround,
    /// Equal space between and at the edges.
    SpaceEvenly,
    /// Stretched to fill.
    Stretch,
}

/// Cross-axis alignment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AlignItems {
    /// At the cross start.
    Start,
    /// At the cross end.
    End,
    /// Centred on the cross axis.
    Center,
    /// Baselines aligned.
    Baseline,
    /// Filling the cross axis.
    Stretch,
}

/// How overflowing content is treated.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Overflow {
    /// Content spills out of the box.
    #[default]
    Visible,
    /// Content is clipped to the box.
    Clip,
    /// Clipped, and scrollable.
    Scroll,
}

impl Overflow {
    /// Whether this value clips descendants to the border box.
    #[must_use]
    pub fn clips(self) -> bool {
        !matches!(self, Self::Visible)
    }
}

/// Whether a box paints.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Visibility {
    /// Painted.
    #[default]
    Visible,
    /// Not painted, but still laid out and still occupying space.
    Hidden,
}

/// Whether glyphs are slanted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum FontStyle {
    /// Upright.
    #[default]
    Normal,
    /// Italic.
    Italic,
}

/// The distance between baselines.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LineHeight {
    /// The font's own metrics decide.
    #[default]
    Normal,
    /// A multiple of the font size. Inherits as the *multiple*, not as the product, which
    /// is why this is not resolved to pixels during the cascade.
    Number(Number),
    /// An absolute length.
    Length(Px),
}

impl LineHeight {
    /// Resolves against a font size, using `normal_ratio` for [`Self::Normal`].
    #[must_use]
    pub fn resolve(self, font_size: f32, normal_ratio: f32) -> f32 {
        match self {
            Self::Normal => font_size * normal_ratio,
            Self::Number(multiple) => font_size * multiple.get(),
            Self::Length(px) => px.get(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn non_finite_lengths_are_rejected_at_construction() {
        assert_eq!(Px::new(f32::NAN).get(), 0.0);
        assert_eq!(Px::new(f32::INFINITY).get(), 0.0);
        assert_eq!(Number::new(f32::NEG_INFINITY).get(), 0.0);
    }

    #[test]
    fn equal_lengths_hash_alike_so_style_can_be_a_map_key() {
        let mut set = HashSet::new();
        set.insert(Px::new(4.0));
        assert!(set.contains(&Px::new(4.0)));
        assert!(!set.contains(&Px::new(4.5)));
    }

    #[test]
    fn negative_zero_is_not_a_separate_key() {
        // -0.0 and 0.0 have different bit patterns, so a bitwise Eq would treat them as
        // distinct and silently halve the interner's hit rate on any style that computes a
        // zero by subtraction.
        assert_eq!(
            Px::new(-0.0),
            Px::new(0.0),
            "negative zero must not be a separate style"
        );
    }

    #[test]
    fn percentages_resolve_against_a_basis() {
        assert_eq!(
            LengthPercentage::Percent(Number::new(50.0)).resolve(200.0),
            100.0
        );
        assert_eq!(LengthPercentage::Px(Px::new(7.0)).resolve(200.0), 7.0);
    }

    #[test]
    fn line_height_normal_uses_the_font_ratio() {
        assert_eq!(LineHeight::Normal.resolve(16.0, 1.2), 19.2);
        assert_eq!(
            LineHeight::Number(Number::new(1.5)).resolve(16.0, 1.2),
            24.0
        );
        assert_eq!(LineHeight::Length(Px::new(20.0)).resolve(16.0, 1.2), 20.0);
    }

    #[test]
    fn sides_map_preserves_edges() {
        let sides = Sides {
            top: 1.0,
            right: 2.0,
            bottom: 3.0,
            left: 4.0,
        };
        let doubled = sides.map(|v: f32| v * 2.0);
        assert_eq!(doubled.top, 2.0);
        assert_eq!(doubled.left, 8.0);
    }
}
