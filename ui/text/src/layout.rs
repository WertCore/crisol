//! The queries ROADMAP §2.5 requires: clusters, cursors, selection, line boxes.

use std::ops::Range;

use crisol_display_list::{Point, Rect, Size};

use crate::model::{Affinity, CaretGeometry, Cursor, Direction, Glyph, Line, ShapedRun};

/// A shaped, wrapped block of text.
///
/// Immutable: editing means shaping again. M6 adds the caching that makes that cheap for a
/// document; until then a keystroke reshapes the paragraph, which is correct and slow rather
/// than fast and wrong.
#[derive(Clone, Debug, PartialEq)]
pub struct TextLayout {
    pub(crate) text: String,
    pub(crate) size: Size,
    pub(crate) lines: Vec<Line>,
    pub(crate) runs: Vec<ShapedRun>,
    pub(crate) glyphs: Vec<Glyph>,
}

impl TextLayout {
    /// The source text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The block's size.
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

        let mut best: Option<(f32, Cursor)> = None;
        let mut consider = |x: f32, cursor: Cursor| {
            let distance = (x - point.x).abs();
            if best.is_none_or(|(closest, _)| distance < closest) {
                best = Some((distance, cursor));
            }
        };

        for run in self.line_runs(line) {
            for glyph in self.run_glyphs(run) {
                // Both edges of every cluster are candidates, so clicking the right half of a
                // character puts the caret after it — which is what a reader expects and what
                // makes double-click-drag feel right.
                let (leading, trailing) = match run.direction {
                    Direction::Ltr => (glyph.position.x, glyph.position.x + glyph.advance),
                    Direction::Rtl => (glyph.position.x + glyph.advance, glyph.position.x),
                };
                consider(leading, Cursor::new(glyph.cluster.start));
                consider(trailing, Cursor::new(glyph.cluster.end));
            }
        }

        match best {
            Some((_, cursor)) => cursor,
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
        let direction = self
            .line_runs(line)
            .first()
            .map_or(Direction::Ltr, |run| run.direction);

        let x = self.caret_x(line, cursor.index, direction);
        CaretGeometry {
            position: Point::new(x, line.bounds.min_y()),
            height: line.bounds.height(),
            line: line_index,
            direction,
        }
    }

    /// The x of a caret at `index` on `line`.
    fn caret_x(&self, line: &Line, index: usize, direction: Direction) -> f32 {
        for run in self.line_runs(line) {
            for glyph in self.run_glyphs(run) {
                if glyph.cluster.start == index {
                    return match run.direction {
                        Direction::Ltr => glyph.position.x,
                        Direction::Rtl => glyph.position.x + glyph.advance,
                    };
                }
                if glyph.cluster.end == index {
                    return match run.direction {
                        Direction::Ltr => glyph.position.x + glyph.advance,
                        Direction::Rtl => glyph.position.x,
                    };
                }
            }
        }
        // Past the end of the line's glyphs, or on a line with none.
        if index <= line.range.start {
            match direction {
                Direction::Ltr => line.bounds.min_x(),
                Direction::Rtl => line.bounds.min_x() + line.width,
            }
        } else {
            match direction {
                Direction::Ltr => line.bounds.min_x() + line.width,
                Direction::Rtl => line.bounds.min_x(),
            }
        }
    }

    // ---- selection ------------------------------------------------------------------

    /// The rectangles covering a byte range, one per line it touches.
    ///
    /// One rectangle per line rather than one per glyph, because a selection highlight is
    /// drawn as a few quads and a per-glyph list would be both slower and visibly seamed
    /// where adjacent rectangles meet.
    ///
    /// An empty range produces no rectangles — a caret is not a selection, and
    /// [`Self::cursor_to_point`] is what draws it.
    #[must_use]
    pub fn selection_rects(&self, range: Range<usize>) -> Vec<Rect> {
        if range.is_empty() {
            return Vec::new();
        }

        let mut rects = Vec::new();
        for line in &self.lines {
            let mut min_x = f32::INFINITY;
            let mut max_x = f32::NEG_INFINITY;

            for run in self.line_runs(line) {
                for glyph in self.run_glyphs(run) {
                    // A cluster is selected when the range covers any of it. Partially
                    // selecting a ligature highlights the whole thing, which is what every
                    // editor does and the only thing that can be drawn.
                    if glyph.cluster.start < range.end && range.start < glyph.cluster.end {
                        let left = glyph.position.x.min(glyph.position.x + glyph.advance);
                        let right = glyph.position.x.max(glyph.position.x + glyph.advance);
                        min_x = min_x.min(left);
                        max_x = max_x.max(right);
                    }
                }
            }

            if min_x <= max_x {
                rects.push(Rect::from_xywh(
                    min_x,
                    line.bounds.min_y(),
                    max_x - min_x,
                    line.bounds.height(),
                ));
            }
        }
        rects
    }
}
