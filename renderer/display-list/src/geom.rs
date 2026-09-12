//! Geometry and colour primitives shared by the whole engine.
//!
//! These live here rather than in a separate `crisol-geom` crate because
//! `crisol-display-list` has no dependencies of its own and sits below everything that
//! needs them — the tree, layout, paint, events and the renderer (DECISIONS D-12).
//!
//! Unless a type says otherwise, coordinates are **logical pixels** with the origin at the
//! top-left, `x` growing right and `y` growing down. The renderer applies the HiDPI scale
//! factor; nothing above the renderer knows about physical pixels.

use core::ops::{Add, Sub};

/// A point in 2D space.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    /// Horizontal coordinate, growing right.
    pub x: f32,
    /// Vertical coordinate, growing down.
    pub y: f32,
}

impl Point {
    /// The origin, `(0, 0)`.
    pub const ZERO: Self = Self { x: 0.0, y: 0.0 };

    /// Constructs a point.
    #[inline]
    #[must_use]
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

impl Add for Point {
    type Output = Self;

    #[inline]
    fn add(self, rhs: Self) -> Self {
        Self::new(self.x + rhs.x, self.y + rhs.y)
    }
}

impl Sub for Point {
    type Output = Self;

    #[inline]
    fn sub(self, rhs: Self) -> Self {
        Self::new(self.x - rhs.x, self.y - rhs.y)
    }
}

/// A width and a height.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Size {
    /// Extent along the x axis.
    pub width: f32,
    /// Extent along the y axis.
    pub height: f32,
}

impl Size {
    /// A zero-area size.
    pub const ZERO: Self = Self {
        width: 0.0,
        height: 0.0,
    };

    /// Constructs a size.
    #[inline]
    #[must_use]
    pub const fn new(width: f32, height: f32) -> Self {
        Self { width, height }
    }

    /// True when either axis is zero or negative, and the size therefore covers no pixels.
    #[inline]
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.width <= 0.0 || self.height <= 0.0
    }
}

/// An axis-aligned rectangle.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    /// Top-left corner.
    pub origin: Point,
    /// Extent from the origin.
    pub size: Size,
}

impl Rect {
    /// A rectangle at the origin covering nothing.
    pub const ZERO: Self = Self {
        origin: Point::ZERO,
        size: Size::ZERO,
    };

    /// Constructs a rectangle from an origin and a size.
    #[inline]
    #[must_use]
    pub const fn new(origin: Point, size: Size) -> Self {
        Self { origin, size }
    }

    /// Constructs a rectangle from loose components.
    #[inline]
    #[must_use]
    pub const fn from_xywh(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self::new(Point::new(x, y), Size::new(width, height))
    }

    /// Left edge.
    #[inline]
    #[must_use]
    pub fn min_x(self) -> f32 {
        self.origin.x
    }

    /// Top edge.
    #[inline]
    #[must_use]
    pub fn min_y(self) -> f32 {
        self.origin.y
    }

    /// Right edge.
    #[inline]
    #[must_use]
    pub fn max_x(self) -> f32 {
        self.origin.x + self.size.width
    }

    /// Bottom edge.
    #[inline]
    #[must_use]
    pub fn max_y(self) -> f32 {
        self.origin.y + self.size.height
    }

    /// Width.
    #[inline]
    #[must_use]
    pub fn width(self) -> f32 {
        self.size.width
    }

    /// Height.
    #[inline]
    #[must_use]
    pub fn height(self) -> f32 {
        self.size.height
    }

    /// Centre point.
    #[inline]
    #[must_use]
    pub fn center(self) -> Point {
        Point::new(
            self.origin.x + self.size.width * 0.5,
            self.origin.y + self.size.height * 0.5,
        )
    }

    /// True when the rectangle covers no pixels.
    #[inline]
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.size.is_empty()
    }

    /// True when `point` lies inside, treating the top and left edges as inside and the
    /// bottom and right edges as outside. That half-open convention is what makes adjacent
    /// rectangles tile without a point belonging to two of them.
    #[inline]
    #[must_use]
    pub fn contains(self, point: Point) -> bool {
        point.x >= self.min_x()
            && point.x < self.max_x()
            && point.y >= self.min_y()
            && point.y < self.max_y()
    }

    /// Moves the rectangle by `offset` without changing its size.
    #[inline]
    #[must_use]
    pub fn translate(self, offset: Point) -> Self {
        Self::new(self.origin + offset, self.size)
    }

    /// Grows the rectangle by `amount` on every edge. A negative amount shrinks it, and the
    /// result is clamped at zero rather than being allowed to invert.
    #[inline]
    #[must_use]
    pub fn inflate(self, amount: f32) -> Self {
        Self::from_xywh(
            self.origin.x - amount,
            self.origin.y - amount,
            (self.size.width + amount * 2.0).max(0.0),
            (self.size.height + amount * 2.0).max(0.0),
        )
    }

    /// Scales origin and size uniformly. Used by the renderer to convert logical pixels to
    /// physical pixels.
    #[inline]
    #[must_use]
    pub fn scale(self, factor: f32) -> Self {
        Self::from_xywh(
            self.origin.x * factor,
            self.origin.y * factor,
            self.size.width * factor,
            self.size.height * factor,
        )
    }

    /// The overlap with `other`, or `None` when they do not overlap.
    #[must_use]
    pub fn intersection(self, other: Self) -> Option<Self> {
        let min_x = self.min_x().max(other.min_x());
        let min_y = self.min_y().max(other.min_y());
        let max_x = self.max_x().min(other.max_x());
        let max_y = self.max_y().min(other.max_y());
        if max_x <= min_x || max_y <= min_y {
            return None;
        }
        Some(Self::from_xywh(min_x, min_y, max_x - min_x, max_y - min_y))
    }

    /// The smallest rectangle containing both. An empty rectangle is absorbed rather than
    /// dragging the result back to the origin.
    #[must_use]
    pub fn union(self, other: Self) -> Self {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        let min_x = self.min_x().min(other.min_x());
        let min_y = self.min_y().min(other.min_y());
        let max_x = self.max_x().max(other.max_x());
        let max_y = self.max_y().max(other.max_y());
        Self::from_xywh(min_x, min_y, max_x - min_x, max_y - min_y)
    }
}

/// Per-corner radii, in CSS order.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Corners {
    /// Top-left radius.
    pub top_left: f32,
    /// Top-right radius.
    pub top_right: f32,
    /// Bottom-right radius.
    pub bottom_right: f32,
    /// Bottom-left radius.
    pub bottom_left: f32,
}

impl Corners {
    /// No rounding.
    pub const ZERO: Self = Self::all(0.0);

    /// The same radius on all four corners.
    #[inline]
    #[must_use]
    pub const fn all(radius: f32) -> Self {
        Self {
            top_left: radius,
            top_right: radius,
            bottom_right: radius,
            bottom_left: radius,
        }
    }

    /// True when no corner is rounded.
    #[inline]
    #[must_use]
    pub fn is_zero(self) -> bool {
        self.top_left <= 0.0
            && self.top_right <= 0.0
            && self.bottom_right <= 0.0
            && self.bottom_left <= 0.0
    }

    /// Scales every radius, for the logical-to-physical conversion.
    #[inline]
    #[must_use]
    pub fn scale(self, factor: f32) -> Self {
        Self {
            top_left: self.top_left * factor,
            top_right: self.top_right * factor,
            bottom_right: self.bottom_right * factor,
            bottom_left: self.bottom_left * factor,
        }
    }

    /// Clamps every radius so that opposing radii on an edge cannot exceed that edge's
    /// length. CSS does this proportionally across the whole box; so do we.
    #[must_use]
    pub fn clamped_to(self, size: Size) -> Self {
        let mut scale = 1.0_f32;
        let mut limit = |sum: f32, extent: f32| {
            if sum > 0.0 && sum > extent {
                scale = scale.min(extent / sum);
            }
        };
        limit(self.top_left + self.top_right, size.width);
        limit(self.bottom_left + self.bottom_right, size.width);
        limit(self.top_left + self.bottom_left, size.height);
        limit(self.top_right + self.bottom_right, size.height);
        self.scale(scale.max(0.0))
    }
}

/// A value per box edge, in CSS order.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Edges {
    /// Top edge.
    pub top: f32,
    /// Right edge.
    pub right: f32,
    /// Bottom edge.
    pub bottom: f32,
    /// Left edge.
    pub left: f32,
}

impl Edges {
    /// Zero on every edge.
    pub const ZERO: Self = Self::all(0.0);

    /// The same value on all four edges.
    #[inline]
    #[must_use]
    pub const fn all(value: f32) -> Self {
        Self {
            top: value,
            right: value,
            bottom: value,
            left: value,
        }
    }

    /// True when every edge is zero or negative.
    #[inline]
    #[must_use]
    pub fn is_zero(self) -> bool {
        self.top <= 0.0 && self.right <= 0.0 && self.bottom <= 0.0 && self.left <= 0.0
    }

    /// Scales every edge, for the logical-to-physical conversion.
    #[inline]
    #[must_use]
    pub fn scale(self, factor: f32) -> Self {
        Self {
            top: self.top * factor,
            right: self.right * factor,
            bottom: self.bottom * factor,
            left: self.left * factor,
        }
    }
}

/// A straight (non-premultiplied) sRGB colour with components in `0.0..=1.0`.
///
/// Straight rather than premultiplied because that is what CSS authors write and what
/// `lightningcss` will hand us. Premultiplication and the sRGB-to-linear conversion happen
/// in the shader (DECISIONS D-15).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Color {
    /// Red, sRGB-encoded.
    pub r: f32,
    /// Green, sRGB-encoded.
    pub g: f32,
    /// Blue, sRGB-encoded.
    pub b: f32,
    /// Alpha, linear.
    pub a: f32,
}

impl Color {
    /// Fully transparent.
    pub const TRANSPARENT: Self = Self::rgba(0.0, 0.0, 0.0, 0.0);
    /// Opaque black.
    pub const BLACK: Self = Self::rgb(0.0, 0.0, 0.0);
    /// Opaque white.
    pub const WHITE: Self = Self::rgb(1.0, 1.0, 1.0);

    /// Constructs an opaque colour.
    #[inline]
    #[must_use]
    pub const fn rgb(r: f32, g: f32, b: f32) -> Self {
        Self::rgba(r, g, b, 1.0)
    }

    /// Constructs a colour with explicit alpha.
    #[inline]
    #[must_use]
    pub const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }

    /// Constructs a colour from 8-bit sRGB components, the form CSS hex notation produces.
    #[inline]
    #[must_use]
    pub fn from_rgba8(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self::rgba(
            f32::from(r) / 255.0,
            f32::from(g) / 255.0,
            f32::from(b) / 255.0,
            f32::from(a) / 255.0,
        )
    }

    /// Converts to 8-bit sRGB components, rounding to nearest.
    #[must_use]
    pub fn to_rgba8(self) -> [u8; 4] {
        let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        [q(self.r), q(self.g), q(self.b), q(self.a)]
    }

    /// The same colour at a different alpha.
    #[inline]
    #[must_use]
    pub fn with_alpha(self, a: f32) -> Self {
        Self { a, ..self }
    }

    /// True when the colour cannot affect the framebuffer and the draw can be skipped.
    #[inline]
    #[must_use]
    pub fn is_transparent(self) -> bool {
        self.a <= 0.0
    }

    /// Component array in the order the shader expects.
    #[inline]
    #[must_use]
    pub fn to_array(self) -> [f32; 4] {
        [self.r, self.g, self.b, self.a]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_is_half_open_so_adjacent_rects_tile() {
        let left = Rect::from_xywh(0.0, 0.0, 10.0, 10.0);
        let right = Rect::from_xywh(10.0, 0.0, 10.0, 10.0);
        let boundary = Point::new(10.0, 5.0);
        assert!(!left.contains(boundary));
        assert!(right.contains(boundary));
    }

    #[test]
    fn intersection_of_disjoint_rects_is_none() {
        let a = Rect::from_xywh(0.0, 0.0, 10.0, 10.0);
        let b = Rect::from_xywh(20.0, 0.0, 10.0, 10.0);
        assert_eq!(a.intersection(b), None);
        // Touching edges share no area either.
        let touching = Rect::from_xywh(10.0, 0.0, 10.0, 10.0);
        assert_eq!(a.intersection(touching), None);
    }

    #[test]
    fn intersection_clips_to_overlap() {
        let a = Rect::from_xywh(0.0, 0.0, 10.0, 10.0);
        let b = Rect::from_xywh(5.0, 5.0, 10.0, 10.0);
        assert_eq!(a.intersection(b), Some(Rect::from_xywh(5.0, 5.0, 5.0, 5.0)));
    }

    #[test]
    fn union_absorbs_empty_rects() {
        let a = Rect::from_xywh(4.0, 4.0, 2.0, 2.0);
        assert_eq!(a.union(Rect::ZERO), a);
        assert_eq!(Rect::ZERO.union(a), a);
    }

    #[test]
    fn inflate_clamps_rather_than_inverting() {
        let a = Rect::from_xywh(0.0, 0.0, 4.0, 4.0);
        let shrunk = a.inflate(-10.0);
        assert_eq!(shrunk.size, Size::ZERO);
    }

    #[test]
    fn corner_radii_are_clamped_proportionally() {
        // Two 30px radii cannot both fit along a 40px edge: CSS scales the whole box by
        // 40/60, not just the offending corner.
        let radii = Corners::all(30.0).clamped_to(Size::new(40.0, 100.0));
        let expected = 30.0 * (40.0 / 60.0);
        assert!((radii.top_left - expected).abs() < 1e-4, "{radii:?}");
        assert!((radii.bottom_right - expected).abs() < 1e-4, "{radii:?}");
    }

    #[test]
    fn corner_radii_that_already_fit_are_untouched() {
        let radii = Corners::all(4.0).clamped_to(Size::new(40.0, 100.0));
        assert_eq!(radii, Corners::all(4.0));
    }

    #[test]
    fn color_round_trips_through_eight_bit() {
        let c = Color::from_rgba8(18, 52, 86, 255);
        assert_eq!(c.to_rgba8(), [18, 52, 86, 255]);
    }
}
