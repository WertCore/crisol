//! Draw commands: the flat, backend-independent output of paint.
//!
//! A [`DisplayList`] is a flat vector in painter's order rather than a tree. Flat means the
//! renderer can batch across sibling and cousin nodes, a damage region can be expressed as a
//! range test instead of a tree walk (M6), and a render snapshot test can be written without
//! constructing a document (DECISIONS D-13).

use crate::geom::{Color, Corners, Edges, Edges4, Rect, Size};

/// An opaque handle to a texture the renderer holds.
///
/// The display list never owns pixels. Paint refers to an image by id; the renderer owns the
/// upload, the atlas and the eviction policy. Keeping bitmap data out of the display list is
/// what lets the list be rebuilt every frame cheaply.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ImageId(pub u64);

/// A filled, optionally rounded, optionally bordered rectangle.
///
/// This one command covers backgrounds, borders, outlines, carets, selection highlights and
/// scrollbar parts — nearly everything a UI draws that is not a glyph or an image. Keeping
/// them a single command is what makes the single instanced draw call of D-14 possible.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RectCommand {
    /// Border box, in logical pixels.
    pub rect: Rect,
    /// Corner radii, already clamped to the box by the producer.
    pub radii: Corners,
    /// Fill colour of the padding box (inside the border).
    pub fill: Color,
    /// Border colour, per edge.
    ///
    /// Per edge rather than one colour because `border-bottom: 1px solid #ddd` is one of the
    /// most common declarations there is, and a single colour renders it in whatever the
    /// *top* edge happened to compute to. Edges meet on the miter diagonal, as CSS says.
    pub border_color: Edges4<Color>,
    /// Per-edge border widths, drawn inside `rect` as CSS `box-sizing: border-box` does.
    pub border_width: Edges,
}

impl RectCommand {
    /// A solid fill with square corners and no border.
    #[must_use]
    pub fn solid(rect: Rect, fill: Color) -> Self {
        Self {
            rect,
            radii: Corners::ZERO,
            fill,
            border_color: Edges4::all(Color::TRANSPARENT),
            border_width: Edges::ZERO,
        }
    }

    /// True when the command cannot change any pixel and can be dropped.
    #[must_use]
    pub fn is_invisible(&self) -> bool {
        if self.rect.is_empty() {
            return true;
        }
        let no_fill = self.fill.is_transparent();
        let no_border = self.border_width.is_zero() || self.border_color.is_transparent();
        no_fill && no_border
    }
}

/// A textured quad.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImageCommand {
    /// Destination rectangle, in logical pixels.
    pub rect: Rect,
    /// Corner radii applied to the destination, so a rounded avatar does not need a mask.
    pub radii: Corners,
    /// Source texture.
    pub image: ImageId,
    /// Source sub-rectangle in normalised `0.0..=1.0` texture coordinates.
    pub uv: Rect,
    /// Multiplied into the sampled texel. Use [`Color::WHITE`] to draw unmodified, and a
    /// white with reduced alpha to fade.
    pub tint: Color,
}

impl ImageCommand {
    /// Draws the whole of `image` into `rect`, unmodified.
    #[must_use]
    pub fn new(rect: Rect, image: ImageId) -> Self {
        Self {
            rect,
            radii: Corners::ZERO,
            image,
            uv: Rect::from_xywh(0.0, 0.0, 1.0, 1.0),
            tint: Color::WHITE,
        }
    }
}

/// A clip region: a rectangle, optionally with rounded corners.
///
/// Rounded clipping is what makes `overflow: hidden` on a card with `border-radius` cut the
/// corners instead of leaving square ones. The bounds become a scissor rectangle and the
/// corners are a per-fragment test in the shader — no stencil buffer, no second render pass,
/// which is what keeps a tile-based GPU happy (DECISIONS D-09, D-14).
///
/// **One set of corners is honoured at a time.** The intersection of two rounded rectangles
/// is not a rounded rectangle, so a rounded clip nested inside another keeps the innermost
/// corners and intersects only the bounds. In practice an outer rounded clip's corners lie
/// outside the inner one anyway; if that assumption ever stops holding, this is the comment
/// to come back to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Clip {
    /// Axis-aligned bounds, already intersected with any enclosing clip.
    pub rect: Rect,
    /// Corner radii. Zero for a plain rectangular clip.
    pub radii: Corners,
    /// The rectangle `radii` were written against.
    ///
    /// Distinct from `rect` because `rect` is the *intersection* with enclosing clips, and a
    /// corner radius belongs to the box it was declared on. Clipping a rounded card to the
    /// viewport must not move its corners.
    pub radii_rect: Rect,
}

impl Clip {
    /// A plain rectangular clip.
    #[must_use]
    pub fn rect(rect: Rect) -> Self {
        Self {
            rect,
            radii: Corners::ZERO,
            radii_rect: rect,
        }
    }

    /// A rounded clip.
    #[must_use]
    pub fn rounded(rect: Rect, radii: Corners) -> Self {
        Self {
            rect,
            radii: radii.clamped_to(rect.size),
            radii_rect: rect,
        }
    }

    /// Whether this clip has corners the shader has to test.
    #[must_use]
    pub fn is_rounded(&self) -> bool {
        !self.radii.is_zero()
    }
}

/// One entry in a display list.
///
/// Clips are a push/pop pair rather than a field on each command so that a subtree's clip is
/// established once instead of being copied onto every descendant's command.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DrawCommand {
    /// Fill a rectangle.
    Rect(RectCommand),
    /// Draw a textured quad.
    Image(ImageCommand),
    /// Intersect the current clip with a rectangle, which may have rounded corners.
    ///
    /// The axis-aligned bounds become a scissor rectangle, which costs nothing on a tiler.
    /// Corner radii are applied per fragment by the shader; see [`Clip`].
    PushClip(Clip),
    /// Restore the clip in force before the matching [`DrawCommand::PushClip`].
    PopClip,
}

/// A frame's worth of drawing, in painter's order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DisplayList {
    commands: Vec<DrawCommand>,
    /// Colour the target is cleared to before any command runs.
    pub background: Color,
    /// Size of the surface this list was built for, in logical pixels.
    pub viewport: Size,
}

impl DisplayList {
    /// An empty list for a viewport of `viewport` logical pixels.
    #[must_use]
    pub fn new(viewport: Size) -> Self {
        Self {
            commands: Vec::new(),
            background: Color::WHITE,
            viewport,
        }
    }

    /// The commands, in painter's order.
    #[must_use]
    pub fn commands(&self) -> &[DrawCommand] {
        &self.commands
    }

    /// Number of commands.
    #[must_use]
    pub fn len(&self) -> usize {
        self.commands.len()
    }

    /// True when there is nothing to draw but the background.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    /// Drops every command, keeping the allocation so the next frame reuses it.
    pub fn clear(&mut self) {
        self.commands.clear();
    }
}

/// Builds a [`DisplayList`], maintaining the clip stack so producers cannot unbalance it.
///
/// The builder tracks the current clip on the CPU and drops commands that fall entirely
/// outside it. Culling here rather than in the renderer means the work is done once, in
/// logical space, by the code that already knows the geometry.
#[derive(Debug)]
pub struct DisplayListBuilder {
    list: DisplayList,
    /// Intersected clip rectangles. The last entry is in force.
    clip_stack: Vec<Clip>,
}

impl DisplayListBuilder {
    /// Starts a list for a viewport of `viewport` logical pixels.
    #[must_use]
    pub fn new(viewport: Size) -> Self {
        Self {
            list: DisplayList::new(viewport),
            clip_stack: Vec::new(),
        }
    }

    /// Sets the colour the target is cleared to.
    pub fn set_background(&mut self, color: Color) {
        self.list.background = color;
    }

    /// The clip currently in force, or `None` when nothing is clipped.
    #[must_use]
    pub fn current_clip(&self) -> Option<Clip> {
        self.clip_stack.last().copied()
    }

    /// True when `rect` is entirely outside the current clip and drawing it would be a
    /// no-op.
    #[must_use]
    fn is_culled(&self, rect: Rect) -> bool {
        if rect.is_empty() {
            return true;
        }
        match self.current_clip() {
            Some(clip) => rect.intersection(clip.rect).is_none(),
            None => false,
        }
    }

    /// Appends a rectangle, dropping it when it is invisible or fully clipped out.
    ///
    /// Returns whether the command was kept. Callers that count what they drew need that;
    /// callers that do not can ignore it.
    pub fn push_rect(&mut self, command: RectCommand) -> bool {
        if command.is_invisible() || self.is_culled(command.rect) {
            return false;
        }
        self.list.commands.push(DrawCommand::Rect(command));
        true
    }

    /// Convenience for a square-cornered solid fill.
    pub fn fill_rect(&mut self, rect: Rect, color: Color) -> bool {
        self.push_rect(RectCommand::solid(rect, color))
    }

    /// Appends a textured quad, dropping it when it is fully clipped out.
    ///
    /// Returns whether the command was kept.
    pub fn push_image(&mut self, command: ImageCommand) -> bool {
        if command.rect.is_empty() || command.tint.is_transparent() || self.is_culled(command.rect)
        {
            return false;
        }
        self.list.commands.push(DrawCommand::Image(command));
        true
    }

    /// Intersects the clip with `rect` until the matching [`Self::pop_clip`].
    ///
    /// Returns `false` when the resulting clip is empty. The caller may use that to skip
    /// painting the subtree entirely, but must still call [`Self::pop_clip`] either way —
    /// the stack entry is pushed regardless so push/pop stay balanced.
    pub fn push_clip(&mut self, rect: Rect) -> bool {
        self.push_rounded_clip(Clip::rect(rect))
    }

    /// Intersects the clip with a rounded rectangle.
    ///
    /// The bounds intersect with the enclosing clip as usual. The radii do not: a rounded
    /// clip inside another rounded clip keeps only the inner one's corners, because the
    /// intersection of two rounded rectangles is not a rounded rectangle. See [`Clip`].
    pub fn push_rounded_clip(&mut self, clip: Clip) -> bool {
        let bounds = match self.current_clip() {
            Some(current) => current.rect.intersection(clip.rect).unwrap_or(Rect::ZERO),
            None => clip.rect,
        };
        let effective = Clip {
            rect: bounds,
            // Keep whichever clip actually has corners; the innermost wins.
            radii: if clip.radii.is_zero() {
                self.current_clip()
                    .map_or(Corners::ZERO, |current| current.radii)
            } else {
                clip.radii
            },
            // The radii belong to the rectangle they were written for, not to the
            // intersection, so the shader needs that box to measure against.
            radii_rect: if clip.radii.is_zero() {
                self.current_clip()
                    .map_or(clip.rect, |current| current.radii_rect)
            } else {
                clip.rect
            },
        };
        self.clip_stack.push(effective);
        self.list.commands.push(DrawCommand::PushClip(effective));
        !effective.rect.is_empty()
    }

    /// Ends the clip opened by the matching [`Self::push_clip`].
    ///
    /// # Panics
    ///
    /// Panics when there is no open clip. An unbalanced stack is a bug in the producer, and
    /// it is far cheaper to find here than as a mis-clipped frame three milestones later.
    pub fn pop_clip(&mut self) {
        assert!(
            self.clip_stack.pop().is_some(),
            "pop_clip without a matching push_clip"
        );
        self.list.commands.push(DrawCommand::PopClip);
    }

    /// Finishes the list.
    ///
    /// # Panics
    ///
    /// Panics when a clip is still open.
    #[must_use]
    pub fn build(self) -> DisplayList {
        assert!(
            self.clip_stack.is_empty(),
            "{} clip(s) left open at build()",
            self.clip_stack.len()
        );
        self.list
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Point;

    fn viewport() -> Size {
        Size::new(100.0, 100.0)
    }

    #[test]
    fn invisible_rects_are_dropped() {
        let mut b = DisplayListBuilder::new(viewport());
        b.fill_rect(Rect::from_xywh(0.0, 0.0, 10.0, 10.0), Color::TRANSPARENT);
        b.fill_rect(Rect::from_xywh(0.0, 0.0, 0.0, 10.0), Color::BLACK);
        assert!(b.build().is_empty());
    }

    #[test]
    fn a_transparent_fill_with_a_border_still_draws() {
        let mut b = DisplayListBuilder::new(viewport());
        b.push_rect(RectCommand {
            rect: Rect::from_xywh(0.0, 0.0, 10.0, 10.0),
            radii: Corners::ZERO,
            fill: Color::TRANSPARENT,
            border_color: Edges4::all(Color::BLACK),
            border_width: Edges::all(1.0),
        });
        assert_eq!(b.build().len(), 1);
    }

    #[test]
    fn clips_intersect_and_cull() {
        let mut b = DisplayListBuilder::new(viewport());
        assert!(b.push_clip(Rect::from_xywh(0.0, 0.0, 50.0, 50.0)));
        assert!(b.push_clip(Rect::from_xywh(40.0, 40.0, 50.0, 50.0)));
        assert_eq!(
            b.current_clip().map(|clip| clip.rect),
            Some(Rect::from_xywh(40.0, 40.0, 10.0, 10.0))
        );
        // Inside the intersection: kept.
        b.fill_rect(Rect::from_xywh(42.0, 42.0, 4.0, 4.0), Color::BLACK);
        // Outside it: dropped without the renderer ever seeing it.
        b.fill_rect(Rect::from_xywh(60.0, 60.0, 4.0, 4.0), Color::BLACK);
        b.pop_clip();
        b.pop_clip();

        let list = b.build();
        let rects = list
            .commands()
            .iter()
            .filter(|c| matches!(c, DrawCommand::Rect(_)))
            .count();
        assert_eq!(rects, 1);
    }

    #[test]
    fn an_empty_clip_reports_itself_but_still_needs_a_pop() {
        let mut b = DisplayListBuilder::new(viewport());
        assert!(b.push_clip(Rect::from_xywh(0.0, 0.0, 10.0, 10.0)));
        assert!(!b.push_clip(Rect::from_xywh(50.0, 50.0, 10.0, 10.0)));
        b.pop_clip();
        b.pop_clip();
        let _ = b.build();
    }

    #[test]
    #[should_panic(expected = "clip(s) left open")]
    fn building_with_an_open_clip_panics() {
        let mut b = DisplayListBuilder::new(viewport());
        b.push_clip(Rect::from_xywh(0.0, 0.0, 10.0, 10.0));
        let _ = b.build();
    }

    #[test]
    #[should_panic(expected = "pop_clip without a matching push_clip")]
    fn popping_an_unopened_clip_panics() {
        DisplayListBuilder::new(viewport()).pop_clip();
    }

    #[test]
    fn painters_order_is_preserved() {
        let mut b = DisplayListBuilder::new(viewport());
        b.fill_rect(Rect::from_xywh(0.0, 0.0, 10.0, 10.0), Color::BLACK);
        b.push_image(ImageCommand::new(
            Rect::from_xywh(1.0, 1.0, 4.0, 4.0),
            ImageId(7),
        ));
        b.fill_rect(Rect::from_xywh(2.0, 2.0, 2.0, 2.0), Color::WHITE);

        let list = b.build();
        assert!(matches!(list.commands()[0], DrawCommand::Rect(_)));
        assert!(matches!(list.commands()[1], DrawCommand::Image(_)));
        assert!(matches!(list.commands()[2], DrawCommand::Rect(_)));
    }

    #[test]
    fn builder_culls_against_the_viewport_only_when_asked() {
        // Nothing clips to the viewport implicitly: a scrolled-out node is the caller's
        // business, and the renderer's scissor still bounds the damage.
        let mut b = DisplayListBuilder::new(viewport());
        b.fill_rect(Rect::from_xywh(500.0, 500.0, 10.0, 10.0), Color::BLACK);
        assert_eq!(b.build().len(), 1);
        assert!(Rect::from_xywh(500.0, 500.0, 10.0, 10.0).contains(Point::new(505.0, 505.0)));
    }
}
