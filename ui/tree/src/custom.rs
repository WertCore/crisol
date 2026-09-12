//! The escape hatch from CSS layout.
//!
//! A PDF page, a spreadsheet grid, a code editor's text surface — these need to opt out of
//! CSS layout entirely while still taking part in hit testing, scrolling, focus, clipping
//! and accessibility. That is [`CustomNode`], and it is designed here at M2 rather than
//! retrofitted later because it is the hardest thing in the tree to add afterwards
//! (DECISIONS D-06, D-19).

use core::fmt::Debug;

use crisol_display_list::{DisplayListBuilder, Point, Rect, Size};

/// What the engine knows about the space available to a node when it asks it to measure.
///
/// Mirrors what `taffy` hands a measure function, so M3 can pass one straight through to
/// the other. The four fields are genuinely distinct: a definite size is a decision the
/// parent has already made, while available space is a budget the node may spend or not.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MeasureConstraints {
    /// Width the parent has already decided, if it has.
    pub width: Option<f32>,
    /// Height the parent has already decided, if it has.
    pub height: Option<f32>,
    /// Width available before overflowing, if bounded.
    pub available_width: Option<f32>,
    /// Height available before overflowing, if bounded.
    pub available_height: Option<f32>,
}

impl MeasureConstraints {
    /// Constraints with a definite size on both axes.
    #[must_use]
    pub fn definite(size: Size) -> Self {
        Self {
            width: Some(size.width),
            height: Some(size.height),
            available_width: Some(size.width),
            available_height: Some(size.height),
        }
    }

    /// A definite width and a free height — the common case for a block in a document.
    #[must_use]
    pub fn definite_width(width: f32) -> Self {
        Self {
            width: Some(width),
            available_width: Some(width),
            ..Self::default()
        }
    }
}

/// What a custom node reports when a point lands inside it.
///
/// The engine cannot interpret `target`; it only carries it back out to whoever installed
/// the node. That is the point: a PDF page resolves a point to an annotation, a grid
/// resolves it to a cell, and the event system does not have to know the difference.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CustomHit {
    /// Opaque identifier, meaningful only to the node that produced it.
    pub target: u64,
    /// The point in the node's own coordinate space, origin at its top-left.
    pub local: Point,
}

/// A node that lays out and paints itself.
///
/// The engine gives a custom node a box and then gets out of the way. In exchange the node
/// must answer four questions, which together are everything the engine needs to treat it
/// like any other node.
///
/// Implementations are stored as `Box<dyn CustomNode>` in the arena, so the trait is
/// object-safe and every method takes `self` by reference.
pub trait CustomNode: Debug + 'static {
    /// Reports the size this node wants, given what the parent can offer.
    ///
    /// Takes constraints rather than a fixed size because a document canvas needs to know
    /// the available width before it can decide its own height. Called during layout, and
    /// possibly more than once per pass.
    fn measure(&mut self, constraints: MeasureConstraints) -> Size;

    /// Notifies the node of the box it actually got, so it can lay out its interior.
    ///
    /// The size may differ from what [`Self::measure`] asked for: the parent has the final
    /// word. The default does nothing, which is right for a node whose interior does not
    /// depend on its size.
    fn layout(&mut self, size: Size) {
        let _ = size;
    }

    /// Emits draw commands for this node.
    ///
    /// `bounds` is the node's box in the display list's coordinate space — absolute logical
    /// pixels, not node-local. The builder's clip is already set to whatever the engine
    /// decided, so a node that draws outside its bounds is clipped, not trusted.
    fn paint(&self, bounds: Rect, builder: &mut DisplayListBuilder);

    /// Resolves a point inside the node to something the node understands.
    ///
    /// `local` has its origin at the node's top-left and is guaranteed to be inside the
    /// node's box; the engine has already done that test. Returning `None` means "this
    /// point is transparent to hit testing", which is how a node declares holes.
    ///
    /// The default treats the whole box as a single opaque target.
    fn hit_test(&self, local: Point) -> Option<CustomHit> {
        Some(CustomHit { target: 0, local })
    }

    /// A label for diagnostics and, from M5, for the accessibility tree.
    fn debug_name(&self) -> &'static str {
        "custom"
    }
}

/// A custom node that fills its box with one colour at a fixed size.
///
/// The stub M2 calls for. It is also the fixture the tree and paint tests use, so it earns
/// its place beyond the milestone.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColorBox {
    /// The size this node reports from `measure`, regardless of the constraints.
    pub size: Size,
    /// The colour it fills with.
    pub color: crisol_display_list::Color,
    /// The value it reports from `hit_test`.
    pub target: u64,
}

impl ColorBox {
    /// A fixed-size box of `color`.
    #[must_use]
    pub fn new(size: Size, color: crisol_display_list::Color) -> Self {
        Self {
            size,
            color,
            target: 0,
        }
    }
}

impl CustomNode for ColorBox {
    fn measure(&mut self, _constraints: MeasureConstraints) -> Size {
        self.size
    }

    fn paint(&self, bounds: Rect, builder: &mut DisplayListBuilder) {
        builder.fill_rect(bounds, self.color);
    }

    fn hit_test(&self, local: Point) -> Option<CustomHit> {
        Some(CustomHit {
            target: self.target,
            local,
        })
    }

    fn debug_name(&self) -> &'static str {
        "ColorBox"
    }
}
