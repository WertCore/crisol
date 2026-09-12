//! Reading a [`ComputedStyle`] through the traits `taffy` asks for.
//!
//! `taffy` 0.14 takes styles through traits rather than a concrete struct, which means the
//! cascade's output can be handed to it directly. No conversion, and — more importantly —
//! no second per-node style allocation to sit alongside the interned one (DECISIONS D-21).
//! Every method here is a one-line translation; the ones taffy's defaults already get right
//! are simply not implemented.

use crisol_style::ComputedStyle;
use crisol_style::values::{
    AlignItems as CrisolAlignItems, Dimension as CrisolDimension, Display,
    FlexDirection as CrisolFlexDirection, FlexWrap as CrisolFlexWrap,
    JustifyContent as CrisolJustifyContent, LengthPercentage as CrisolLp,
    Overflow as CrisolOverflow, Position as CrisolPosition, Sides,
};
use taffy::geometry::{Point, Rect, Size};
use taffy::style::{
    AlignContent, AlignItems, AlignSelf, BlockContainerStyle, BlockItemStyle, BoxGenerationMode,
    BoxSizing, CoreStyle, Dimension, FlexDirection, FlexWrap, FlexboxContainerStyle,
    FlexboxItemStyle, JustifyContent, LengthPercentage, LengthPercentageAuto, Overflow, Position,
};

/// A borrowed computed style, viewed as a taffy style.
///
/// Carries one bit the cascade cannot know: whether the node is *replaced* — a custom node
/// that measures itself (DECISIONS D-06). A replaced element with `width: auto` takes its
/// intrinsic width rather than stretching to its container, which is what makes a PDF page
/// its own width instead of the window's.
#[derive(Clone, Copy, Debug)]
pub struct StyleRef<'a> {
    /// The computed style.
    pub style: &'a ComputedStyle,
    /// Whether this node measures itself.
    pub replaced: bool,
}

impl<'a> StyleRef<'a> {
    /// A view of an ordinary element's style.
    #[must_use]
    pub fn new(style: &'a ComputedStyle) -> Self {
        Self {
            style,
            replaced: false,
        }
    }

    /// A view of a self-measuring node's style.
    #[must_use]
    pub fn replaced(style: &'a ComputedStyle) -> Self {
        Self {
            style,
            replaced: true,
        }
    }
}

impl CoreStyle for StyleRef<'_> {
    type CustomIdent = String;

    fn box_generation_mode(&self) -> BoxGenerationMode {
        if self.style.display == Display::None {
            BoxGenerationMode::None
        } else {
            BoxGenerationMode::Normal
        }
    }

    fn is_block(&self) -> bool {
        // A replaced element is not part of its parent's block formatting context, which is
        // what stops block layout from stretching it to the container width.
        !self.replaced && self.style.display == Display::Block
    }

    fn is_compressible_replaced(&self) -> bool {
        self.replaced
    }

    fn box_sizing(&self) -> BoxSizing {
        // CSS defaults to `content-box`, and every design system in existence sets
        // `border-box` on everything as its first line. The property is not in M3's subset,
        // so rather than support half of it, the engine picks the one authors actually want
        // and says so. Revisit when `box-sizing` itself is implemented.
        BoxSizing::BorderBox
    }

    fn overflow(&self) -> Point<Overflow> {
        Point {
            x: overflow(self.style.overflow_x),
            y: overflow(self.style.overflow_y),
        }
    }

    fn position(&self) -> Position {
        match self.style.position {
            CrisolPosition::Relative => Position::Relative,
            CrisolPosition::Absolute => Position::Absolute,
        }
    }

    fn inset(&self) -> Rect<LengthPercentageAuto> {
        rect(self.style.inset, dimension_auto)
    }

    fn size(&self) -> Size<Dimension> {
        Size {
            width: dimension(self.style.width),
            height: dimension(self.style.height),
        }
    }

    fn min_size(&self) -> Size<LengthPercentageAuto> {
        Size {
            width: dimension_auto(self.style.min_width),
            height: dimension_auto(self.style.min_height),
        }
    }

    fn max_size(&self) -> Size<LengthPercentageAuto> {
        Size {
            width: dimension_auto(self.style.max_width),
            height: dimension_auto(self.style.max_height),
        }
    }

    fn margin(&self) -> Rect<LengthPercentageAuto> {
        rect(self.style.margin, dimension_auto)
    }

    fn padding(&self) -> Rect<LengthPercentage> {
        rect(self.style.padding, length_percentage)
    }

    fn border(&self) -> Rect<LengthPercentage> {
        rect(self.style.border_width, |px| {
            LengthPercentage::length(px.get())
        })
    }
}

impl FlexboxContainerStyle for StyleRef<'_> {
    fn flex_direction(&self) -> FlexDirection {
        match self.style.flex_direction {
            CrisolFlexDirection::Row => FlexDirection::Row,
            CrisolFlexDirection::RowReverse => FlexDirection::RowReverse,
            CrisolFlexDirection::Column => FlexDirection::Column,
            CrisolFlexDirection::ColumnReverse => FlexDirection::ColumnReverse,
        }
    }

    fn flex_wrap(&self) -> FlexWrap {
        match self.style.flex_wrap {
            CrisolFlexWrap::NoWrap => FlexWrap::NoWrap,
            CrisolFlexWrap::Wrap => FlexWrap::Wrap,
            CrisolFlexWrap::WrapReverse => FlexWrap::WrapReverse,
        }
    }

    fn gap(&self) -> Size<LengthPercentage> {
        Size {
            // `column-gap` is the gap *between columns*, which is a horizontal distance.
            width: length_percentage(self.style.column_gap),
            height: length_percentage(self.style.row_gap),
        }
    }

    fn align_content(&self) -> Option<AlignContent> {
        self.style.align_content.map(align_content)
    }

    fn align_items(&self) -> Option<AlignItems> {
        self.style.align_items.map(align_items)
    }

    fn justify_content(&self) -> Option<JustifyContent> {
        self.style.justify_content.map(align_content)
    }
}

impl FlexboxItemStyle for StyleRef<'_> {
    fn flex_basis(&self) -> Dimension {
        dimension(self.style.flex_basis)
    }

    fn flex_grow(&self) -> f32 {
        self.style.flex_grow.get()
    }

    fn flex_shrink(&self) -> f32 {
        self.style.flex_shrink.get()
    }

    fn align_self(&self) -> Option<AlignSelf> {
        self.style.align_self.map(align_items)
    }
}

impl BlockContainerStyle for StyleRef<'_> {
    fn align_content(&self) -> Option<AlignContent> {
        self.style.align_content.map(align_content)
    }
}

impl BlockItemStyle for StyleRef<'_> {
    fn is_table(&self) -> bool {
        // Tables as layout are out of scope (ROADMAP §1).
        false
    }
}

// ---- conversions ---------------------------------------------------------------------

fn overflow(value: CrisolOverflow) -> Overflow {
    match value {
        CrisolOverflow::Visible => Overflow::Visible,
        CrisolOverflow::Clip => Overflow::Hidden,
        CrisolOverflow::Scroll => Overflow::Scroll,
    }
}

fn length_percentage(value: CrisolLp) -> LengthPercentage {
    match value {
        CrisolLp::Px(px) => LengthPercentage::length(px.get()),
        // taffy takes a fraction; CSS writes a percent.
        CrisolLp::Percent(fraction) => LengthPercentage::percent(fraction.get() / 100.0),
    }
}

fn dimension(value: CrisolDimension) -> Dimension {
    match value {
        CrisolDimension::Auto => Dimension::auto(),
        CrisolDimension::Length(CrisolLp::Px(px)) => Dimension::length(px.get()),
        CrisolDimension::Length(CrisolLp::Percent(f)) => Dimension::percent(f.get() / 100.0),
    }
}

fn dimension_auto(value: CrisolDimension) -> LengthPercentageAuto {
    match value {
        CrisolDimension::Auto => LengthPercentageAuto::auto(),
        CrisolDimension::Length(CrisolLp::Px(px)) => LengthPercentageAuto::length(px.get()),
        CrisolDimension::Length(CrisolLp::Percent(f)) => {
            LengthPercentageAuto::percent(f.get() / 100.0)
        }
    }
}

fn rect<T, U>(sides: Sides<T>, convert: impl Fn(T) -> U) -> Rect<U>
where
    T: Copy,
{
    Rect {
        left: convert(sides.left),
        right: convert(sides.right),
        top: convert(sides.top),
        bottom: convert(sides.bottom),
    }
}

/// taffy distinguishes safe from unsafe alignment; CSS's bare keywords are the unsafe ones,
/// which overflow the container rather than snapping back to the start.
fn align_content(value: CrisolJustifyContent) -> AlignContent {
    match value {
        CrisolJustifyContent::Start => AlignContent::START,
        CrisolJustifyContent::End => AlignContent::END,
        CrisolJustifyContent::Center => AlignContent::CENTER,
        CrisolJustifyContent::SpaceBetween => AlignContent::SPACE_BETWEEN,
        CrisolJustifyContent::SpaceAround => AlignContent::SPACE_AROUND,
        CrisolJustifyContent::SpaceEvenly => AlignContent::SPACE_EVENLY,
        CrisolJustifyContent::Stretch => AlignContent::STRETCH,
    }
}

fn align_items(value: CrisolAlignItems) -> AlignItems {
    match value {
        CrisolAlignItems::Start => AlignItems::START,
        CrisolAlignItems::End => AlignItems::END,
        CrisolAlignItems::Center => AlignItems::CENTER,
        CrisolAlignItems::Baseline => AlignItems::BASELINE,
        CrisolAlignItems::Stretch => AlignItems::STRETCH,
    }
}
