//! The queries ROADMAP §2.5 requires: clusters, cursors, selection, line boxes.

use std::ops::Range;

use crisol_display_list::{Point, Rect, Size};

use crate::model::{Affinity, CaretGeometry, Cursor, Direction, Glyph, Line, ShapedRun};

/// How far apart two selected stretches may be and still be drawn as one rectangle.
///
/// Glyphs inside a run abut exactly; this only absorbs the rounding between two runs that
/// meet, so that a highlight is not cut by a hairline gap.
const MERGE_TOLERANCE: f32 = 0.01;

/// A shaped, wrapped block of text.
///
/// Immutable: editing means shaping again. M6 adds the caching that makes that cheap for a
/// document; until then a keystroke reshapes the paragraph, which is correct and slow rather
/// than fast and wrong.
#[derive(Debug)]
pub struct TextLayout {
    pub(crate) text: String,
    pub(crate) size: Size,
    pub(crate) lines: Vec<Line>,
    pub(crate) runs: Vec<ShapedRun>,
    pub(crate) glyphs: Vec<Glyph>,
    /// The shaper's own buffer, kept so a GPU text backend can rasterise from it.
    ///
    /// Not `Clone` or `PartialEq`, which is why this type is not either: a shaped buffer is
    /// a cache of rasterisation state, and duplicating it would duplicate that.
    pub(crate) buffer: Option<cosmic_text::Buffer>,
}

impl TextLayout {
    /// A layout with no glyphs: one line, the right height, nothing drawn.
    ///
    /// What an empty string produces, and what shaping without a font produces. A caret has
    /// to go somewhere and an empty paragraph is still one line tall, so this is a real
    /// answer rather than a failure.
    pub(crate) fn empty(text: &str, style: &crate::TextStyle, width: Option<f32>) -> Self {
        let height = style.line_height.max(1.0);
        Self {
            text: text.to_owned(),
            size: Size::new(0.0, height),
            lines: vec![Line {
                range: 0..0,
                bounds: Rect::from_xywh(0.0, 0.0, width.unwrap_or(0.0), height),
                baseline: height * 0.8,
                width: 0.0,
                runs: 0..0,
            }],
            runs: Vec::new(),
            glyphs: Vec::new(),
            buffer: None,
        }
    }

    /// The source text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The shaper's own buffer.
    ///
    /// An integration point for a GPU text backend — `crisol-text-gpu` hands it to glyphon
    /// — and not part of the stable surface. `None` when there was nothing to shape.
    #[doc(hidden)]
    #[must_use]
    pub fn cosmic_buffer(&self) -> Option<&cosmic_text::Buffer> {
        self.buffer.as_ref()
    }

    /// The size of the text itself: the widest line by the total height.
    ///
    /// Deliberately not the width the block was given. This is what a layout engine asks for
    /// when measuring, and answering with the container's width would make every text node
    /// fill its parent. [`Line::bounds`] is the other one — the full-width line *box*, which
    /// is what hit testing needs so that clicking past the end of a short line still finds
    /// it.
    #[must_use]
    pub fn size(&self) -> Size {
        self.size
    }

    /// The visual lines, top to bottom.
    #[must_use]
    pub fn lines(&self) -> &[Line] {
        &self.lines
    }

    /// The shaped runs, in visual order within each line.
    #[must_use]
    pub fn runs(&self) -> &[ShapedRun] {
        &self.runs
    }

    /// Every positioned glyph.
    #[must_use]
    pub fn glyphs(&self) -> &[Glyph] {
        &self.glyphs
    }

    /// The glyphs belonging to a run.
    #[must_use]
    pub fn run_glyphs(&self, run: &ShapedRun) -> &[Glyph] {
        &self.glyphs[run.glyphs.clone()]
    }

    /// The runs belonging to a line.
    #[must_use]
    pub fn line_runs(&self, line: &Line) -> &[ShapedRun] {
        &self.runs[line.runs.clone()]
    }

    // ---- clusters -------------------------------------------------------------------

    /// The cluster containing `index`, as a byte range.
    ///
    /// A cluster is the smallest run of text a caret may not be placed inside: a base
    /// character with its combining marks, an emoji with its modifiers, a ligature. Returns
    /// an empty range at `index` when nothing covers it, which happens past the end and
    /// inside whitespace that produced no glyph.
    #[must_use]
    pub fn cluster_at(&self, index: usize) -> Range<usize> {
        self.glyphs
            .iter()
            .find(|glyph| glyph.cluster.contains(&index))
            .map_or(index..index, |glyph| glyph.cluster.clone())
    }

    /// The next position a caret may occupy after `index`.
    ///
    /// Steps over a whole cluster, so an emoji with a skin tone modifier is one press of the
    /// arrow key rather than three that leave the text looking broken in between.
    #[must_use]
    pub fn next_cluster(&self, index: usize) -> usize {
        if index >= self.text.len() {
            return self.text.len();
        }
        let cluster = self.cluster_at(index);
        if cluster.end > index {
            return cluster.end;
        }
        // No glyph covers this offset — whitespace, or text that shaped away. Fall back to
        // the next character boundary, which is always a legal caret position.
        self.text[index..]
            .char_indices()
            .nth(1)
            .map_or(self.text.len(), |(offset, _)| index + offset)
    }

    /// The previous position a caret may occupy before `index`.
    #[must_use]
    pub fn prev_cluster(&self, index: usize) -> usize {
        if index == 0 {
            return 0;
        }
        if let Some(glyph) = self
            .glyphs
            .iter()
            .find(|glyph| glyph.cluster.end == index && glyph.cluster.start < index)
        {
            return glyph.cluster.start;
        }
        self.text[..index]
            .char_indices()
            .next_back()
            .map_or(0, |(offset, _)| offset)
    }

    /// Whether `index` is a position a caret may occupy.
    #[must_use]
    pub fn is_cluster_boundary(&self, index: usize) -> bool {
        if index == 0 || index == self.text.len() {
            return true;
        }
        if !self.text.is_char_boundary(index) {
            return false;
        }
        // Inside a glyph's cluster is not a boundary; anywhere else on a char boundary is.
        !self
            .glyphs
            .iter()
            .any(|glyph| glyph.cluster.start < index && index < glyph.cluster.end)
    }

    // ---- lines ----------------------------------------------------------------------

    /// The line containing `y`, clamped to the first and last.
    ///
    /// Clamped rather than optional because a click above or below the block still has to
    /// put the caret somewhere, and "the nearest line" is what every editor does.
    #[must_use]
    pub fn line_at_y(&self, y: f32) -> usize {
        for (index, line) in self.lines.iter().enumerate() {
            if y < line.bounds.max_y() {
                return index;
            }
        }
        self.lines.len().saturating_sub(1)
    }

    /// The line containing a byte offset.
    #[must_use]
    pub fn line_at_index(&self, index: usize) -> usize {
        for (i, line) in self.lines.iter().enumerate() {
            if index < line.range.end {
                return i;
            }
        }
        self.lines.len().saturating_sub(1)
    }

    // ---- hit testing ----------------------------------------------------------------

    /// The caret position nearest to `point`.
    ///
    /// Never fails: a point outside the block resolves to the nearest edge, because a drag
    /// that leaves the text still has to select something sensible.
    #[must_use]
    pub fn point_to_cursor(&self, point: Point) -> Cursor {
        let line_index = self.line_at_y(point.y);
        let Some(line) = self.lines.get(line_index) else {
            return Cursor::new(0);
        };

        // `(distance, inside)`: a candidate from the run the point actually lies in beats
        // one that is merely as close. That is what settles a direction boundary, where the
        // two runs meeting there offer the same x for two different byte offsets — clicking
        // inside the Hebrew of `ab שלום cd` must not land the caret in the Latin.
        let mut best: Option<(f32, bool, Cursor)> = None;
        let mut consider = |x: f32, inside: bool, cursor: Cursor| {
            let distance = (x - point.x).abs();
            let better = best.is_none_or(|(closest, was_inside, _)| {
                if inside == was_inside {
                    distance < closest
                } else {
                    inside
                }
            });
            if better {
                best = Some((distance, inside, cursor));
            }
        };

        for run in self.line_runs(line) {
            let inside = self
                .run_extent(run)
                .is_some_and(|(left, right)| point.x >= left && point.x <= right);
            for glyph in self.run_glyphs(run) {
                // Both edges of every cluster are candidates, so clicking the right half of a
                // character puts the caret after it — which is what a reader expects and what
                // makes double-click-drag feel right.
                //
                // The affinity records which side the caret came from, so that feeding it
                // back to `cursor_to_point` returns it to where it was clicked rather than to
                // the other place the same offset sits.
                let (leading, trailing) = match run.direction {
                    Direction::Ltr => (glyph.position.x, glyph.position.x + glyph.advance),
                    Direction::Rtl => (glyph.position.x + glyph.advance, glyph.position.x),
                };
                consider(leading, inside, Cursor::new(glyph.cluster.start));
                consider(trailing, inside, Cursor::upstream(glyph.cluster.end));
            }
        }

        match best {
            Some((_, _, cursor)) => cursor,
            // A line with no glyphs: an empty paragraph, or a line holding only the
            // whitespace that caused the wrap.
            None => Cursor::new(line.range.start),
        }
    }

    /// Where to draw the caret for a cursor.
    ///
    /// Affinity decides which line an offset at a soft wrap belongs to. `Upstream` puts the
    /// caret at the end of the previous line, `Downstream` at the start of the next — the
    /// two places the same byte offset legitimately is.
    #[must_use]
    pub fn cursor_to_point(&self, cursor: Cursor) -> CaretGeometry {
        let mut line_index = self.line_at_index(cursor.index);

        // At a soft wrap the offset is the end of one line and the start of the next.
        if cursor.affinity == Affinity::Upstream
            && line_index > 0
            && self.lines[line_index].range.start == cursor.index
        {
            line_index -= 1;
        }

        let line = &self.lines[line_index];
        let (x, direction) = self.caret_at(line, cursor.index, cursor.affinity);
        CaretGeometry {
            position: Point::new(x, line.bounds.min_y()),
            height: line.bounds.height(),
            line: line_index,
            direction,
        }
    }

    /// Where the caret sits for `index` on `line`, and the direction of the text it is in.
    ///
    /// At a direction boundary one byte offset has **two** positions on screen: the visual
    /// end of the run before it and the visual start of the run after it. In `ab שלום cd`
    /// the offset at the start of the Hebrew is both just after the space on the left and at
    /// the right-hand end of the Hebrew, because that is where reading it begins. Affinity is
    /// what chooses, and it is the same question it answers at a soft wrap.
    fn caret_at(&self, line: &Line, index: usize, affinity: Affinity) -> (f32, Direction) {
        // The run that *starts* here, and the run that *ends* here. Away from a boundary
        // both describe the same place and it does not matter which is used.
        let mut starts_here = None;
        let mut ends_here = None;

        for run in self.line_runs(line) {
            for glyph in self.run_glyphs(run) {
                if glyph.cluster.start == index && starts_here.is_none() {
                    let x = match run.direction {
                        Direction::Ltr => glyph.position.x,
                        Direction::Rtl => glyph.position.x + glyph.advance,
                    };
                    starts_here = Some((x, run.direction));
                }
                if glyph.cluster.end == index && ends_here.is_none() {
                    let x = match run.direction {
                        Direction::Ltr => glyph.position.x + glyph.advance,
                        Direction::Rtl => glyph.position.x,
                    };
                    ends_here = Some((x, run.direction));
                }
            }
        }

        let chosen = match affinity {
            Affinity::Downstream => starts_here.or(ends_here),
            Affinity::Upstream => ends_here.or(starts_here),
        };
        if let Some(found) = chosen {
            return found;
        }

        // Past the end of the line's glyphs, or on a line with none.
        let direction = self
            .line_runs(line)
            .first()
            .map_or(Direction::Ltr, |run| run.direction);
        let x = if index <= line.range.start {
            match direction {
                Direction::Ltr => line.bounds.min_x(),
                Direction::Rtl => line.bounds.min_x() + line.width,
            }
        } else {
            match direction {
                Direction::Ltr => line.bounds.min_x() + line.width,
                Direction::Rtl => line.bounds.min_x(),
            }
        };
        (x, direction)
    }

    /// The visual span a run occupies, as `(left, right)`.
    fn run_extent(&self, run: &ShapedRun) -> Option<(f32, f32)> {
        let mut left = f32::INFINITY;
        let mut right = f32::NEG_INFINITY;
        for glyph in self.run_glyphs(run) {
            left = left.min(glyph.position.x.min(glyph.position.x + glyph.advance));
            right = right.max(glyph.position.x.max(glyph.position.x + glyph.advance));
        }
        (left <= right).then_some((left, right))
    }

    // ---- selection ------------------------------------------------------------------

    /// The rectangles covering a byte range.
    ///
    /// One rectangle per *visually contiguous stretch* on each line, rather than one per
    /// glyph: a highlight is drawn as a few quads, and a per-glyph list would be both slower
    /// and visibly seamed where adjacent rectangles meet.
    ///
    /// A line usually yields exactly one. It yields more where the selected text is not
    /// contiguous on screen, which bidirectional text makes ordinary — in `ab שלום cd`,
    /// selecting `b` and the first Hebrew letter selects two pieces with four characters
    /// between them, and taking the span from the leftmost to the rightmost would highlight
    /// text the user did not select.
    ///
    /// An empty range produces no rectangles — a caret is not a selection, and
    /// [`Self::cursor_to_point`] is what draws it.
    #[must_use]
    pub fn selection_rects(&self, range: Range<usize>) -> Vec<Rect> {
        if range.is_empty() {
            return Vec::new();
        }

        let mut rects = Vec::new();
        let mut spans: Vec<(f32, f32)> = Vec::new();

        for line in &self.lines {
            spans.clear();
            for run in self.line_runs(line) {
                for glyph in self.run_glyphs(run) {
                    // A cluster is selected when the range covers any of it. Partially
                    // selecting a ligature highlights the whole thing, which is what every
                    // editor does and the only thing that can be drawn.
                    if glyph.cluster.start < range.end && range.start < glyph.cluster.end {
                        let left = glyph.position.x.min(glyph.position.x + glyph.advance);
                        let right = glyph.position.x.max(glyph.position.x + glyph.advance);
                        spans.push((left, right));
                    }
                }
            }
            if spans.is_empty() {
                continue;
            }

            // Merge what touches. Glyphs within a run abut exactly, so the usual result is
            // one span per line; the tolerance keeps a rounding difference from leaving a
            // hairline gap through a highlight.
            spans.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut merged: Vec<(f32, f32)> = Vec::with_capacity(spans.len());
            for &(left, right) in &spans {
                match merged.last_mut() {
                    Some(previous) if left <= previous.1 + MERGE_TOLERANCE => {
                        previous.1 = previous.1.max(right);
                    }
                    _ => merged.push((left, right)),
                }
            }

            rects.extend(merged.into_iter().map(|(left, right)| {
                Rect::from_xywh(
                    left,
                    line.bounds.min_y(),
                    right - left,
                    line.bounds.height(),
                )
            }));
        }
        rects
    }
}
