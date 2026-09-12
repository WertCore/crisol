//! Applying one declaration to a computed style.
//!
//! This is where specified values become computed ones: `em` and `rem` resolve to pixels,
//! keywords become enums, and anything outside M3's property subset is ignored. Percentages
//! survive, because they resolve against a containing block layout has not measured yet.
//!
//! Every arm is a longhand. `crisol-css` expanded the shorthands at parse time, so `margin`
//! never reaches here — only `margin-top` and its three siblings.

use lightningcss::properties::Property;
use lightningcss::properties::align as lc_align;
use lightningcss::properties::display as lc_display;
use lightningcss::properties::flex as lc_flex;
use lightningcss::properties::font as lc_font;
use lightningcss::properties::overflow as lc_overflow;
use lightningcss::properties::position as lc_position;
use lightningcss::properties::size as lc_size;
use lightningcss::traits::ToCss;
use lightningcss::values::color::CssColor;
use lightningcss::values::length::{LengthPercentage, LengthPercentageOrAuto, LengthValue};
use lightningcss::values::percentage::DimensionPercentage;

use crate::computed::{ComputedStyle, INITIAL_FONT_SIZE};
use crate::values::{
    AlignItems, Color, Dimension, Display, FlexDirection, FlexWrap, FontStyle, JustifyContent,
    LengthPercentage as Lp, LineHeight, Number, Overflow, Position, Px, Visibility,
};

/// Applies one declaration, leaving `style` unchanged when the property or value is outside
/// the supported subset.
///
/// `parent` is needed for `em`, which resolves against the *parent's* font size when it
/// appears in `font-size` itself, and against this element's everywhere else.
#[allow(
    clippy::too_many_lines,
    reason = "one arm per property; splitting it would hide the table"
)]
pub(crate) fn apply(style: &mut ComputedStyle, property: &Property<'_>, parent: &ComputedStyle) {
    // Every length except the one inside `font-size` resolves against this element's own
    // font size, which earlier declarations in this same cascade may already have set.
    let em = style.font_size.get();
    let rem = INITIAL_FONT_SIZE;

    match property {
        // ---- box ----------------------------------------------------------------
        Property::Display(value) => {
            if let Some(display) = convert_display(value) {
                style.display = display;
            }
        }
        Property::Position(value) => {
            style.position = match value {
                lc_position::Position::Absolute | lc_position::Position::Fixed => {
                    Position::Absolute
                }
                _ => Position::Relative,
            };
        }
        Property::Top(v) => style.inset.top = dimension(v, em, rem),
        Property::Right(v) => style.inset.right = dimension(v, em, rem),
        Property::Bottom(v) => style.inset.bottom = dimension(v, em, rem),
        Property::Left(v) => style.inset.left = dimension(v, em, rem),

        Property::Width(v) => style.width = size(v, em, rem),
        Property::Height(v) => style.height = size(v, em, rem),
        Property::MinWidth(v) => style.min_width = size(v, em, rem),
        Property::MinHeight(v) => style.min_height = size(v, em, rem),
        Property::MaxWidth(v) => style.max_width = max_size(v, em, rem),
        Property::MaxHeight(v) => style.max_height = max_size(v, em, rem),

        Property::MarginTop(v) => style.margin.top = dimension(v, em, rem),
        Property::MarginRight(v) => style.margin.right = dimension(v, em, rem),
        Property::MarginBottom(v) => style.margin.bottom = dimension(v, em, rem),
        Property::MarginLeft(v) => style.margin.left = dimension(v, em, rem),

        // Padding cannot be `auto`, but lightningcss types it the same as margin.
        Property::PaddingTop(v) => style.padding.top = padding(v, em, rem),
        Property::PaddingRight(v) => style.padding.right = padding(v, em, rem),
        Property::PaddingBottom(v) => style.padding.bottom = padding(v, em, rem),
        Property::PaddingLeft(v) => style.padding.left = padding(v, em, rem),

        Property::BorderTopWidth(v) => style.border_width.top = border_width(v, em, rem),
        Property::BorderRightWidth(v) => style.border_width.right = border_width(v, em, rem),
        Property::BorderBottomWidth(v) => style.border_width.bottom = border_width(v, em, rem),
        Property::BorderLeftWidth(v) => style.border_width.left = border_width(v, em, rem),

        Property::BorderTopColor(v) => style.border_color.top = color(v, style, parent),
        Property::BorderRightColor(v) => style.border_color.right = color(v, style, parent),
        Property::BorderBottomColor(v) => style.border_color.bottom = color(v, style, parent),
        Property::BorderLeftColor(v) => style.border_color.left = color(v, style, parent),

        Property::BorderTopLeftRadius(v, _) => {
            style.border_radius.top_left = length_percentage(&v.0, em, rem);
        }
        Property::BorderTopRightRadius(v, _) => {
            style.border_radius.top_right = length_percentage(&v.0, em, rem);
        }
        Property::BorderBottomRightRadius(v, _) => {
            style.border_radius.bottom_right = length_percentage(&v.0, em, rem);
        }
        Property::BorderBottomLeftRadius(v, _) => {
            style.border_radius.bottom_left = length_percentage(&v.0, em, rem);
        }

        // ---- flex ---------------------------------------------------------------
        Property::FlexDirection(value, _) => {
            style.flex_direction = match value {
                lc_flex::FlexDirection::Row => FlexDirection::Row,
                lc_flex::FlexDirection::RowReverse => FlexDirection::RowReverse,
                lc_flex::FlexDirection::Column => FlexDirection::Column,
                lc_flex::FlexDirection::ColumnReverse => FlexDirection::ColumnReverse,
            };
        }
        Property::FlexWrap(value, _) => {
            style.flex_wrap = match value {
                lc_flex::FlexWrap::NoWrap => FlexWrap::NoWrap,
                lc_flex::FlexWrap::Wrap => FlexWrap::Wrap,
                lc_flex::FlexWrap::WrapReverse => FlexWrap::WrapReverse,
            };
        }
        Property::FlexGrow(value, _) => style.flex_grow = Number::new(*value),
        Property::FlexShrink(value, _) => style.flex_shrink = Number::new(*value),
        Property::FlexBasis(value, _) => style.flex_basis = dimension(value, em, rem),

        Property::RowGap(value) => style.row_gap = gap(value, em, rem),
        Property::ColumnGap(value) => style.column_gap = gap(value, em, rem),

        Property::JustifyContent(value, _) => {
            style.justify_content = convert_justify_content(value);
        }
        Property::AlignContent(value, _) => {
            style.align_content = convert_align_content(value);
        }
        Property::AlignItems(value, _) => style.align_items = convert_align_items(value),
        Property::AlignSelf(value, _) => style.align_self = convert_align_self(value),

        // ---- paint --------------------------------------------------------------
        Property::Color(value) => style.color = color(value, style, parent),
        Property::BackgroundColor(value) => {
            style.background_color = color(value, style, parent);
        }
        Property::Opacity(value) => {
            style.opacity = Number::new(alpha(value)).clamped_unit();
        }
        Property::OverflowX(value) => style.overflow_x = convert_overflow(value),
        Property::OverflowY(value) => style.overflow_y = convert_overflow(value),
        Property::Visibility(value) => {
            style.visibility = match value {
                lc_display::Visibility::Visible => Visibility::Visible,
                // `collapse` differs from `hidden` only for table rows, which do not exist
                // here (ROADMAP §1), so it is the same thing.
                _ => Visibility::Hidden,
            };
        }

        // ---- text ---------------------------------------------------------------
        Property::FontSize(value) => {
            // The one place `em` means the *parent's* font size rather than this element's.
            if let Some(size) = font_size(value, parent.font_size.get(), rem) {
                style.font_size = Px::new(size);
            }
        }
        Property::FontWeight(value) => {
            if let Some(weight) = font_weight(value, parent.font_weight) {
                style.font_weight = weight;
            }
        }
        Property::FontStyle(value) => {
            style.font_style = match value {
                lc_font::FontStyle::Normal => FontStyle::Normal,
                _ => FontStyle::Italic,
            };
        }
        Property::FontFamily(families) => {
            style.font_family = families
                .iter()
                .map(|family| match family {
                    // `FamilyName` keeps its string private; serializing is the supported
                    // way to read it, and font names are short.
                    lc_font::FontFamily::FamilyName(name) => crisol_tree::Atom::new(
                        &name
                            .to_css_string(lightningcss::printer::PrinterOptions::default())
                            .unwrap_or_default(),
                    ),
                    lc_font::FontFamily::Generic(generic) => {
                        crisol_tree::Atom::new(generic_name(*generic))
                    }
                })
                .collect();
        }
        Property::LineHeight(value) => {
            style.line_height = match value {
                lc_font::LineHeight::Normal => LineHeight::Normal,
                lc_font::LineHeight::Number(n) => LineHeight::Number(Number::new(*n)),
                lc_font::LineHeight::Length(lp) => match length_percentage(lp, em, rem) {
                    Lp::Px(px) => LineHeight::Length(px),
                    // A percentage line-height computes to a length immediately, against
                    // this element's own font size — unlike every other percentage, it does
                    // not wait for layout.
                    Lp::Percent(fraction) => {
                        LineHeight::Length(Px::new(fraction.get() / 100.0 * em))
                    }
                },
            };
        }

        // Everything else is outside M3's subset. Ignoring it is the CSS-correct answer:
        // a declaration the engine does not implement behaves as if it were not written.
        _ => {}
    }
}

// ---- conversions ---------------------------------------------------------------------

fn convert_display(value: &lc_display::Display) -> Option<Display> {
    let lc_display::Display::Pair(pair) = value else {
        // `Keyword` is `none` or `contents`. Only `none` is meaningful here.
        return match value {
            lc_display::Display::Keyword(lc_display::DisplayKeyword::None) => Some(Display::None),
            _ => None,
        };
    };
    match pair.inside {
        lc_display::DisplayInside::Flex(_) => Some(Display::Flex),
        lc_display::DisplayInside::Flow | lc_display::DisplayInside::FlowRoot => {
            Some(Display::Block)
        }
        // Grid needs the `grid-template-*` properties to mean anything and they are not in
        // M3's subset; tables and ruby are out of scope entirely (ROADMAP §1). Leaving the
        // property alone is better than silently laying the box out as something it is not.
        _ => None,
    }
}

fn convert_justify_content(value: &lc_align::JustifyContent) -> Option<JustifyContent> {
    match value {
        lc_align::JustifyContent::Normal => None,
        lc_align::JustifyContent::ContentDistribution(d) => Some(distribution(*d)),
        lc_align::JustifyContent::ContentPosition { value, .. } => Some(content_position(*value)),
        // Physical keywords. There is one writing mode, so `left` is the start.
        lc_align::JustifyContent::Left { .. } => Some(JustifyContent::Start),
        lc_align::JustifyContent::Right { .. } => Some(JustifyContent::End),
    }
}

fn convert_align_content(value: &lc_align::AlignContent) -> Option<JustifyContent> {
    match value {
        lc_align::AlignContent::Normal => None,
        lc_align::AlignContent::ContentDistribution(d) => Some(distribution(*d)),
        lc_align::AlignContent::ContentPosition { value, .. } => Some(content_position(*value)),
        // Baseline alignment of whole lines is not something taffy implements; the start is
        // where an unaligned line goes anyway.
        lc_align::AlignContent::BaselinePosition(_) => Some(JustifyContent::Start),
    }
}

fn distribution(value: lc_align::ContentDistribution) -> JustifyContent {
    match value {
        lc_align::ContentDistribution::SpaceBetween => JustifyContent::SpaceBetween,
        lc_align::ContentDistribution::SpaceAround => JustifyContent::SpaceAround,
        lc_align::ContentDistribution::SpaceEvenly => JustifyContent::SpaceEvenly,
        lc_align::ContentDistribution::Stretch => JustifyContent::Stretch,
    }
}

fn content_position(value: lc_align::ContentPosition) -> JustifyContent {
    match value {
        lc_align::ContentPosition::Center => JustifyContent::Center,
        lc_align::ContentPosition::Start | lc_align::ContentPosition::FlexStart => {
            JustifyContent::Start
        }
        lc_align::ContentPosition::End | lc_align::ContentPosition::FlexEnd => JustifyContent::End,
    }
}

fn convert_align_items(value: &lc_align::AlignItems) -> Option<AlignItems> {
    match value {
        lc_align::AlignItems::Normal => None,
        lc_align::AlignItems::Stretch => Some(AlignItems::Stretch),
        lc_align::AlignItems::BaselinePosition(_) => Some(AlignItems::Baseline),
        lc_align::AlignItems::SelfPosition { value, .. } => Some(self_position(*value)),
    }
}

fn convert_align_self(value: &lc_align::AlignSelf) -> Option<AlignItems> {
    match value {
        lc_align::AlignSelf::Auto | lc_align::AlignSelf::Normal => None,
        lc_align::AlignSelf::Stretch => Some(AlignItems::Stretch),
        lc_align::AlignSelf::BaselinePosition(_) => Some(AlignItems::Baseline),
        lc_align::AlignSelf::SelfPosition { value, .. } => Some(self_position(*value)),
    }
}

fn self_position(value: lc_align::SelfPosition) -> AlignItems {
    match value {
        lc_align::SelfPosition::Center => AlignItems::Center,
        lc_align::SelfPosition::Start
        | lc_align::SelfPosition::FlexStart
        | lc_align::SelfPosition::SelfStart => AlignItems::Start,
        lc_align::SelfPosition::End
        | lc_align::SelfPosition::FlexEnd
        | lc_align::SelfPosition::SelfEnd => AlignItems::End,
    }
}

fn convert_overflow(value: &lc_overflow::OverflowKeyword) -> Overflow {
    match value {
        lc_overflow::OverflowKeyword::Visible => Overflow::Visible,
        lc_overflow::OverflowKeyword::Hidden | lc_overflow::OverflowKeyword::Clip => Overflow::Clip,
        lc_overflow::OverflowKeyword::Scroll | lc_overflow::OverflowKeyword::Auto => {
            Overflow::Scroll
        }
    }
}

fn generic_name(generic: lc_font::GenericFontFamily) -> &'static str {
    match generic {
        lc_font::GenericFontFamily::Serif => "serif",
        lc_font::GenericFontFamily::SansSerif => "sans-serif",
        lc_font::GenericFontFamily::Cursive => "cursive",
        lc_font::GenericFontFamily::Fantasy => "fantasy",
        lc_font::GenericFontFamily::Monospace => "monospace",
        lc_font::GenericFontFamily::SystemUI => "system-ui",
        _ => "sans-serif",
    }
}

// ---- lengths -------------------------------------------------------------------------

/// Resolves an absolute or font-relative length to pixels, or `None` for `calc()` this
/// engine cannot evaluate without layout.
fn to_px(value: &LengthValue, em: f32, rem: f32) -> Option<f32> {
    match value {
        LengthValue::Em(n) => Some(n * em),
        LengthValue::Rem(n) => Some(n * rem),
        other => other.to_px(),
    }
}

fn length_percentage(value: &LengthPercentage, em: f32, rem: f32) -> Lp {
    match value {
        DimensionPercentage::Dimension(length) => {
            Lp::Px(Px::new(to_px(length, em, rem).unwrap_or(0.0)))
        }
        DimensionPercentage::Percentage(p) => Lp::Percent(Number::new(p.0 * 100.0)),
        // `calc()` mixing lengths and percentages cannot be reduced before layout knows the
        // containing block. Treating it as zero is wrong but bounded; evaluating it is M3's
        // successor's problem.
        DimensionPercentage::Calc(_) => Lp::ZERO,
    }
}

/// Padding is typed like margin upstream but cannot be `auto`; `auto` computes to zero.
fn padding(value: &LengthPercentageOrAuto, em: f32, rem: f32) -> Lp {
    match value {
        LengthPercentageOrAuto::Auto => Lp::ZERO,
        LengthPercentageOrAuto::LengthPercentage(lp) => length_percentage(lp, em, rem),
    }
}

fn dimension(value: &LengthPercentageOrAuto, em: f32, rem: f32) -> Dimension {
    match value {
        LengthPercentageOrAuto::Auto => Dimension::Auto,
        LengthPercentageOrAuto::LengthPercentage(lp) => {
            Dimension::Length(length_percentage(lp, em, rem))
        }
    }
}

fn size(value: &lc_size::Size, em: f32, rem: f32) -> Dimension {
    match value {
        lc_size::Size::Auto => Dimension::Auto,
        lc_size::Size::LengthPercentage(lp) => Dimension::Length(length_percentage(lp, em, rem)),
        // `min-content` and friends are real layout modes taffy supports, but they are not
        // in M3's subset; `auto` is the honest approximation.
        _ => Dimension::Auto,
    }
}

fn max_size(value: &lc_size::MaxSize, em: f32, rem: f32) -> Dimension {
    match value {
        lc_size::MaxSize::None => Dimension::Auto,
        lc_size::MaxSize::LengthPercentage(lp) => Dimension::Length(length_percentage(lp, em, rem)),
        _ => Dimension::Auto,
    }
}

fn border_width(
    value: &lightningcss::properties::border::BorderSideWidth,
    em: f32,
    rem: f32,
) -> Px {
    use lightningcss::properties::border::BorderSideWidth;
    Px::new(match value {
        // The keyword widths CSS defines. Browsers agree on these numbers.
        BorderSideWidth::Thin => 1.0,
        BorderSideWidth::Medium => 3.0,
        BorderSideWidth::Thick => 5.0,
        BorderSideWidth::Length(length) => match length {
            lightningcss::values::length::Length::Value(v) => to_px(v, em, rem).unwrap_or(0.0),
            lightningcss::values::length::Length::Calc(_) => 0.0,
        },
    })
}

fn gap(value: &lc_align::GapValue, em: f32, rem: f32) -> Lp {
    match value {
        lc_align::GapValue::Normal => Lp::ZERO,
        lc_align::GapValue::LengthPercentage(lp) => length_percentage(lp, em, rem),
    }
}

fn alpha(value: &lightningcss::values::alpha::AlphaValue) -> f32 {
    value.0
}

/// Converts a colour, resolving `currentColor` against the style being computed.
///
/// `currentColor` on `color` itself resolves against the *parent's* colour, because the
/// element's own is exactly what is being decided.
fn color(value: &CssColor, style: &ComputedStyle, parent: &ComputedStyle) -> Color {
    match value {
        CssColor::CurrentColor => style.color,
        other => match lightningcss::values::color::RGBA::try_from(other) {
            Ok(rgba) => Color::rgba(rgba.red, rgba.green, rgba.blue, rgba.alpha),
            // `system-color()` and `light-dark()` need context this engine has no source
            // for. Falling back to the inherited text colour keeps text readable, which is
            // the failure mode that matters.
            Err(()) => parent.color,
        },
    }
}

fn font_size(value: &lc_font::FontSize, parent_size: f32, rem: f32) -> Option<f32> {
    match value {
        lc_font::FontSize::Length(lp) => match lp {
            DimensionPercentage::Dimension(length) => to_px(length, parent_size, rem),
            DimensionPercentage::Percentage(p) => Some(p.0 * parent_size),
            DimensionPercentage::Calc(_) => None,
        },
        // `larger` and `smaller` are relative to the parent; the absolute keywords are a
        // fixed scale. One step is 1.2x, which is the ratio the CSS scale uses.
        lc_font::FontSize::Relative(relative) => Some(match relative {
            lc_font::RelativeFontSize::Larger => parent_size * 1.2,
            lc_font::RelativeFontSize::Smaller => parent_size / 1.2,
        }),
        lc_font::FontSize::Absolute(absolute) => Some(absolute_font_size(absolute, rem)),
    }
}

fn absolute_font_size(value: &lc_font::AbsoluteFontSize, rem: f32) -> f32 {
    // The CSS absolute size scale, as multiples of the initial font size.
    let ratio = match value {
        lc_font::AbsoluteFontSize::XXSmall => 3.0 / 5.0,
        lc_font::AbsoluteFontSize::XSmall => 3.0 / 4.0,
        lc_font::AbsoluteFontSize::Small => 8.0 / 9.0,
        lc_font::AbsoluteFontSize::Medium => 1.0,
        lc_font::AbsoluteFontSize::Large => 6.0 / 5.0,
        lc_font::AbsoluteFontSize::XLarge => 3.0 / 2.0,
        lc_font::AbsoluteFontSize::XXLarge => 2.0,
        _ => 3.0,
    };
    rem * ratio
}

fn font_weight(value: &lc_font::FontWeight, parent_weight: u16) -> Option<u16> {
    match value {
        lc_font::FontWeight::Absolute(absolute) => match absolute {
            lc_font::AbsoluteFontWeight::Weight(n) => Some(n.round().clamp(1.0, 1000.0) as u16),
            lc_font::AbsoluteFontWeight::Normal => Some(400),
            lc_font::AbsoluteFontWeight::Bold => Some(700),
        },
        // `bolder` and `lighter` step along the CSS ladder relative to the inherited weight.
        lc_font::FontWeight::Bolder => Some(match parent_weight {
            0..=349 => 400,
            350..=549 => 700,
            _ => 900,
        }),
        lc_font::FontWeight::Lighter => Some(match parent_weight {
            0..=549 => 100,
            550..=749 => 400,
            _ => 700,
        }),
    }
}
