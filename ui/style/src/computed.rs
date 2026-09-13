//! The computed style of one node.

use std::sync::Arc;

use crisol_tree::{Atom, BoxStyle};

use crate::values::{
    AlignItems, Color, CornerRadii, CursorIcon, Dimension, Display, FlexDirection, FlexWrap,
    FontStyle, JustifyContent, LengthPercentage, LineHeight, Number, Overflow, Position, Px, Sides,
    Visibility,
};

/// The font size an unstyled document starts at, and what `rem` resolves against.
pub const INITIAL_FONT_SIZE: f32 = 16.0;

/// What `line-height: normal` means, as a multiple of the font size.
///
/// A real implementation asks the font for its ascent, descent and line gap. Until M4 loads
/// fonts there is nothing to ask, and 1.2 is the ratio browsers fall back to.
pub const NORMAL_LINE_HEIGHT_RATIO: f32 = 1.2;

/// Every property in M3's subset, resolved for one node.
///
/// `Eq + Hash` on purpose: two nodes with identical computed style must share one
/// allocation, and that is enforced by using this as a hash map key
/// (ROADMAP §M3, [`crate::StyleInterner`]). Per-node computed style at document scale is
/// hundreds of megabytes, which contradicts the entire product thesis.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ComputedStyle {
    // ---- box --------------------------------------------------------------------
    /// How the box participates in layout.
    pub display: Display,
    /// How the box is positioned.
    pub position: Position,
    /// Offsets from the box's normal position, for `position`.
    pub inset: Sides<Dimension>,
    /// Preferred width.
    pub width: Dimension,
    /// Preferred height.
    pub height: Dimension,
    /// Lower bound on width.
    pub min_width: Dimension,
    /// Lower bound on height.
    pub min_height: Dimension,
    /// Upper bound on width.
    pub max_width: Dimension,
    /// Upper bound on height.
    pub max_height: Dimension,
    /// Space outside the border box.
    pub margin: Sides<Dimension>,
    /// Space between the border and the content.
    pub padding: Sides<LengthPercentage>,
    /// Border thickness, inside the border box.
    pub border_width: Sides<Px>,

    // ---- flex -------------------------------------------------------------------
    /// Main-axis direction.
    pub flex_direction: FlexDirection,
    /// Whether items wrap.
    pub flex_wrap: FlexWrap,
    /// Share of free space taken.
    pub flex_grow: Number,
    /// Share of overflow given up.
    pub flex_shrink: Number,
    /// Starting main size.
    pub flex_basis: Dimension,
    /// Main-axis distribution.
    pub justify_content: Option<JustifyContent>,
    /// Cross-axis alignment of items.
    pub align_items: Option<AlignItems>,
    /// Cross-axis alignment of this item, overriding its parent's `align-items`.
    pub align_self: Option<AlignItems>,
    /// Cross-axis distribution of lines.
    pub align_content: Option<JustifyContent>,
    /// Gap between rows.
    pub row_gap: LengthPercentage,
    /// Gap between columns.
    pub column_gap: LengthPercentage,

    // ---- paint ------------------------------------------------------------------
    /// Text colour. Inherited.
    pub color: Color,
    /// Background of the border box.
    pub background_color: Color,
    /// Border colour per edge.
    pub border_color: Sides<Color>,
    /// Corner radii.
    pub border_radius: CornerRadii,
    /// Opacity of the whole subtree, `0.0..=1.0`.
    pub opacity: Number,
    /// The pointer shape over this element. Inherited.
    pub cursor: CursorIcon,
    /// Horizontal overflow handling.
    pub overflow_x: Overflow,
    /// Vertical overflow handling.
    pub overflow_y: Overflow,
    /// Whether the box paints. Inherited.
    pub visibility: Visibility,

    // ---- text -------------------------------------------------------------------
    /// Font family stack, most preferred first. Inherited.
    pub font_family: Arc<[Atom]>,
    /// Font size in pixels. Inherited, and what `em` resolves against for descendants.
    pub font_size: Px,
    /// Font weight, `1..=1000`. Inherited.
    pub font_weight: u16,
    /// Whether glyphs are slanted. Inherited.
    pub font_style: FontStyle,
    /// Baseline distance. Inherited as written, not as a resolved length.
    pub line_height: LineHeight,
}

impl Default for ComputedStyle {
    /// The initial value of every property, which is what an element with no matching rules
    /// and no parent computes to.
    fn default() -> Self {
        Self {
            display: Display::Block,
            position: Position::Relative,
            inset: Sides::all(Dimension::Auto),
            width: Dimension::Auto,
            height: Dimension::Auto,
            min_width: Dimension::Auto,
            min_height: Dimension::Auto,
            max_width: Dimension::Auto,
            max_height: Dimension::Auto,
            margin: Sides::all(Dimension::px(0.0)),
            padding: Sides::all(LengthPercentage::ZERO),
            border_width: Sides::all(Px::ZERO),

            flex_direction: FlexDirection::Row,
            flex_wrap: FlexWrap::NoWrap,
            flex_grow: Number::ZERO,
            flex_shrink: Number::ONE,
            flex_basis: Dimension::Auto,
            justify_content: None,
            align_items: None,
            align_self: None,
            align_content: None,
            row_gap: LengthPercentage::ZERO,
            column_gap: LengthPercentage::ZERO,

            color: Color::BLACK,
            background_color: Color::TRANSPARENT,
            border_color: Sides::all(Color::BLACK),
            border_radius: CornerRadii::default(),
            opacity: Number::ONE,
            cursor: CursorIcon::Auto,
            overflow_x: Overflow::Visible,
            overflow_y: Overflow::Visible,
            visibility: Visibility::Visible,

            font_family: Arc::from([] as [Atom; 0]),
            font_size: Px::new(INITIAL_FONT_SIZE),
            font_weight: 400,
            font_style: FontStyle::Normal,
            line_height: LineHeight::Normal,
        }
    }
}

impl ComputedStyle {
    /// Whether these two styles would lay out identically.
    ///
    /// True when they differ only in properties that change how a box is *drawn* and not
    /// where it is or how big it is — so the caller can mark `PAINT` and leave `LAYOUT`
    /// alone. Toggling `.done` on a todo row, where the rule is
    /// `li.done span.label { color: … }`, should not relay out the row.
    ///
    /// **Written as "overwrite the paint-only fields, then compare everything" rather than
    /// as a list of layout fields to compare, and that is the whole point.** The two spellings
    /// behave identically today and fail in opposite directions tomorrow: a field added to
    /// `ComputedStyle` and forgotten here is treated as layout-affecting, which costs a
    /// relayout nobody needed. Under the other spelling the same omission would skip a
    /// relayout that *was* needed, and the result is a box that silently keeps a stale size.
    /// One is a performance bug; the other is a wrong picture.
    ///
    /// The clone is a memcpy plus one `Arc` bump on `font_family`, and it is paid only for
    /// nodes whose style actually changed — the caller asks this question inside a branch
    /// that already knows the two styles differ.
    #[must_use]
    pub fn layout_eq(&self, other: &Self) -> bool {
        let mut probe = self.clone();
        probe.color = other.color;
        probe.background_color = other.background_color;
        probe.border_color = other.border_color;
        probe.border_radius = other.border_radius;
        probe.opacity = other.opacity;
        probe.cursor = other.cursor;
        probe.visibility = other.visibility;
        probe == *other
    }

    /// The style a child starts from before its own declarations are applied.
    ///
    /// Inherited properties carry down; everything else resets to its initial value. Which
    /// properties inherit is a fixed list in CSS, and it is exactly the text-ish ones: a
    /// child of a styled box does not get its parent's width.
    #[must_use]
    pub fn inherited_from(parent: &Self) -> Self {
        Self {
            color: parent.color,
            visibility: parent.visibility,
            // Inherited, as CSS says: setting `cursor: pointer` on a button has to reach the
            // text inside it, or the pointer flickers back to an arrow over the label.
            cursor: parent.cursor,
            font_family: Arc::clone(&parent.font_family),
            font_size: parent.font_size,
            font_weight: parent.font_weight,
            font_style: parent.font_style,
            // Inherited as the *multiple*, not as the product: a child with a larger font
            // gets a proportionally larger line box, which is the whole point of writing
            // `line-height: 1.5`.
            line_height: parent.line_height,
            ..Self::default()
        }
    }

    /// Whether this box generates no box at all.
    #[must_use]
    pub fn is_none(&self) -> bool {
        self.display == Display::None
    }

    /// Whether descendants are clipped to the border box.
    #[must_use]
    pub fn clips_children(&self) -> bool {
        self.overflow_x.clips() || self.overflow_y.clips()
    }

    /// The resolved line height in pixels.
    #[must_use]
    pub fn resolved_line_height(&self) -> f32 {
        self.line_height
            .resolve(self.font_size.get(), NORMAL_LINE_HEIGHT_RATIO)
    }

    /// Projects onto the text system's style.
    ///
    /// `line_height` is resolved here rather than carried as a multiple, because the shaper
    /// needs a number of pixels and this is the last place that knows the font size it is a
    /// multiple *of*.
    #[must_use]
    pub fn to_text_style(&self) -> crisol_text::TextStyle {
        crisol_text::TextStyle {
            font_size: self.font_size.get(),
            line_height: self.resolved_line_height(),
            families: self
                .font_family
                .iter()
                .map(|family| family.as_str().to_owned())
                .collect(),
            weight: self.font_weight,
            italic: self.font_style == FontStyle::Italic,
        }
    }

    /// Projects onto the paint-facing struct the tree carries.
    ///
    /// `border_box` is needed because corner radii can be percentages, and a percentage
    /// radius is only meaningful once layout has decided the box.
    #[must_use]
    pub fn to_box_style(&self, border_box: crisol_display_list::Size) -> BoxStyle {
        let radius = |value: LengthPercentage, basis: f32| value.resolve(basis);
        BoxStyle {
            background: self.background_color.into(),
            border_color: crisol_display_list::Edges4 {
                top: self.border_color.top.into(),
                right: self.border_color.right.into(),
                bottom: self.border_color.bottom.into(),
                left: self.border_color.left.into(),
            },
            border_width: crisol_display_list::Edges {
                top: self.border_width.top.get(),
                right: self.border_width.right.get(),
                bottom: self.border_width.bottom.get(),
                left: self.border_width.left.get(),
            },
            radii: crisol_display_list::Corners {
                top_left: radius(self.border_radius.top_left, border_box.width),
                top_right: radius(self.border_radius.top_right, border_box.width),
                bottom_right: radius(self.border_radius.bottom_right, border_box.width),
                bottom_left: radius(self.border_radius.bottom_left, border_box.width),
            },
            text_color: self.color.into(),
            generates_box: !self.is_none(),
            clips_children: self.clips_children(),
            scrolls: self.overflow_x == Overflow::Scroll || self.overflow_y == Overflow::Scroll,
            visible: self.visibility == Visibility::Visible,
        }
    }
}
