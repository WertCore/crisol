//! The vocabulary of the text API.
//!
//! These types are the contract, not an implementation detail (ROADMAP §2.5). Every serious
//! web editor — Google Docs, Figma, Notion — abandons `contenteditable` and renders text
//! itself, which means the webview was never providing the thing those applications need.
//! What they need is exactly this: where the glyphs are, where a click lands, where a
//! selection draws, and where the lines sit.
//!
//! **Positions are byte offsets into the source string**, always on a `char` boundary. Not
//! `char` counts, because indexing a `String` by `char` is O(n) and every caller already has
//! the bytes; not UTF-16 offsets, because nothing here is JavaScript yet and M16 can convert
//! at the boundary where it matters.

use std::ops::Range;

use crisol_display_list::{Point, Rect};

#[allow(unused_imports, reason = "referenced only from doc links")]
use crate::TextLayout;

/// Which side of a boundary a cursor sits on.
///
/// One byte offset can be two places on screen. At a soft wrap, the offset after the last
/// character of a line is both the end of that line and the start of the next; at a
/// direction change in bidirectional text, it is both the end of one run and the start of
/// another. Affinity is how a caller says which one it means, and losing it is why so many
/// editors put the caret in the wrong place at the end of a wrapped line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Affinity {
    /// Belongs with the text before it: the end of the previous line or run.
    Upstream,
    /// Belongs with the text after it: the start of the next line or run.
    #[default]
    Downstream,
}

/// A position in the text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Cursor {
    /// Byte offset into the source string, on a `char` boundary.
    pub index: usize,
    /// Which side of a line or direction boundary this position sits on.
    pub affinity: Affinity,
}

impl Cursor {
    /// A cursor at `index`, belonging with the text after it.
    #[must_use]
    pub fn new(index: usize) -> Self {
        Self {
            index,
            affinity: Affinity::Downstream,
        }
    }

    /// A cursor at `index`, belonging with the text before it.
    #[must_use]
    pub fn upstream(index: usize) -> Self {
        Self {
            index,
            affinity: Affinity::Upstream,
        }
    }
}

/// Writing direction of a run.
///
/// Present from the first version even though bidirectional layout is deferred to M8
/// (ROADMAP §M4): an API that assumes left-to-right cannot be extended to one that does not,
/// because every caller will have baked the assumption into its own arithmetic.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Direction {
    /// Left to right.
    #[default]
    Ltr,
    /// Right to left.
    Rtl,
}

impl Direction {
    /// True when text in this direction advances towards decreasing x.
    #[must_use]
    pub fn is_rtl(self) -> bool {
        self == Self::Rtl
    }
}

/// One positioned glyph.
#[derive(Clone, Debug, PartialEq)]
pub struct Glyph {
    /// Index of the glyph within its font. Meaningless without [`Self::font`].
    pub id: u16,
    /// The font this glyph came from, which is not necessarily the one the author asked for
    /// — fallback happens per character.
    pub font: FontId,
    /// Byte range of the cluster this glyph belongs to.
    ///
    /// A range rather than an offset because the mapping is not one to one in either
    /// direction: `é` written as `e` plus a combining accent is two characters and one
    /// glyph, and a ligature is several characters and one glyph, while an emoji with a skin
    /// tone modifier can be several characters and several glyphs that must not be split.
    pub cluster: Range<usize>,
    /// Position of the glyph's origin, relative to the text block's top-left.
    pub position: Point,
    /// How far the pen moves after drawing this glyph.
    pub advance: f32,
    /// Font size in pixels.
    pub font_size: f32,
}

/// A font, as the text system knows it.
///
/// Opaque: it identifies a face the engine has loaded, and carries no meaning outside the
/// [`FontSystem`](crate::FontSystem) that produced it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FontId(pub(crate) u64);

/// A run of glyphs sharing a font, a size and a direction.
///
/// The unit a renderer wants: everything in a run can be drawn with one atlas and one
/// direction of travel.
#[derive(Clone, Debug, PartialEq)]
pub struct ShapedRun {
    /// Byte range of the source text this run covers.
    pub range: Range<usize>,
    /// Which way the text in this run reads.
    pub direction: Direction,
    /// The font every glyph in this run came from.
    pub font: FontId,
    /// Font size in pixels.
    pub font_size: f32,
    /// Index range into [`TextLayout::glyphs`].
    pub glyphs: Range<usize>,
    /// The line this run belongs to, as an index into [`TextLayout::lines`].
    pub line: usize,
}

/// One visual line, after wrapping.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    /// Byte range of the source text on this line.
    pub range: Range<usize>,
    /// The line box: full width of the laid-out block, full line height.
    ///
    /// A *box*, not a tight bound on the glyphs. Selection highlights and hit testing both
    /// want the box — clicking in the empty space to the right of a short line should put
    /// the caret at the end of that line, not miss.
    pub bounds: Rect,
    /// Distance from the top of the line box down to the baseline.
    pub baseline: f32,
    /// Width actually covered by glyphs, which is at most `bounds.width()`.
    pub width: f32,
    /// Index range into [`TextLayout::runs`].
    pub runs: Range<usize>,
}

impl Line {
    /// The y coordinate of this line's baseline, in the text block's space.
    #[must_use]
    pub fn baseline_y(&self) -> f32 {
        self.bounds.min_y() + self.baseline
    }
}

/// Where a caret should be drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CaretGeometry {
    /// Top of the caret, in the text block's space.
    pub position: Point,
    /// How tall to draw it: the line height, not the glyph height, so the caret is the same
    /// size on an empty line as on a full one.
    pub height: f32,
    /// The line the caret is on.
    pub line: usize,
    /// Direction of the text the caret sits in, which is what tells a renderer whether to
    /// draw a direction indicator on it.
    pub direction: Direction,
}
