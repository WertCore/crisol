//! Turning a string into a [`TextLayout`].

use std::collections::HashMap;
use std::ops::Range;

use cosmic_text::{Attrs, Buffer, Family, Metrics, Shaping, Weight, Wrap};
use crisol_display_list::{Point, Rect, Size};

use crate::TextLayout;
use crate::model::{Direction, FontId, Glyph, Line, ShapedRun};

/// Loaded fonts, and the shaper that uses them.
///
/// Expensive to create — it enumerates the system's fonts — and cheap to reuse, so an
/// application builds one and keeps it. Wrapping cosmic-text's `FontSystem` rather than
/// re-exporting it keeps the public API free of a dependency that will be swapped or
/// upgraded underneath.
pub struct FontSystem {
    inner: cosmic_text::FontSystem,
    /// Stable small ids for the font database's own, so [`FontId`] means something a caller
    /// can compare and store.
    ids: HashMap<cosmic_text::fontdb::ID, FontId>,
    next_id: u64,
}

impl std::fmt::Debug for FontSystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FontSystem")
            .field("faces", &self.inner.db().len())
            .finish_non_exhaustive()
    }
}

impl Default for FontSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl FontSystem {
    /// Loads the system's fonts.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: cosmic_text::FontSystem::new(),
            ids: HashMap::new(),
            next_id: 0,
        }
    }

    /// A font system with no fonts at all, holding only what is later loaded into it.
    ///
    /// What a deterministic test or a self-contained application binary wants: the same
    /// glyphs on every machine, rather than whatever the host happens to have installed.
    ///
    /// Note that cosmic-text's `new_with_fonts(empty())` is *not* this — it still scans the
    /// system, and quietly gave a "no fonts" test 983 faces to shape with.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            inner: cosmic_text::FontSystem::new_with_locale_and_db(
                "en-US".to_owned(),
                cosmic_text::fontdb::Database::new(),
            ),
            ids: HashMap::new(),
            next_id: 0,
        }
    }

    /// Adds a font from memory, for an application that ships its own.
    pub fn load(&mut self, data: Vec<u8>) {
        self.inner.db_mut().load_font_data(data);
    }

    /// The shaper's own font system.
    ///
    /// An integration point for a GPU text backend, which has to rasterise with the same
    /// fonts the text was shaped with. Not part of the stable surface.
    #[doc(hidden)]
    pub fn cosmic_font_system(&mut self) -> &mut cosmic_text::FontSystem {
        &mut self.inner
    }

    /// How many faces are loaded.
    #[must_use]
    pub fn face_count(&self) -> usize {
        self.inner.db().len()
    }

    /// True when there are no fonts at all, and every shaping attempt will produce nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.face_count() == 0
    }

    fn font_id(&mut self, id: cosmic_text::fontdb::ID) -> FontId {
        if let Some(existing) = self.ids.get(&id) {
            return *existing;
        }
        let assigned = FontId(self.next_id);
        self.next_id += 1;
        self.ids.insert(id, assigned);
        assigned
    }
}

/// How text should be shaped and wrapped.
#[derive(Clone, Debug, PartialEq)]
pub struct TextStyle {
    /// Font size in pixels.
    pub font_size: f32,
    /// Distance between baselines, in pixels.
    pub line_height: f32,
    /// Family stack, most preferred first. Empty means the system default.
    pub families: Vec<String>,
    /// Weight, `1..=1000`.
    pub weight: u16,
    /// Whether glyphs are slanted.
    pub italic: bool,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            font_size: 16.0,
            line_height: 16.0 * 1.2,
            families: Vec::new(),
            weight: 400,
            italic: false,
        }
    }
}

/// Where a line is allowed to break.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Wrapping {
    /// Never break. The block is as wide as its longest line.
    None,
    /// Break between words.
    #[default]
    Word,
    /// Break between words, and inside a word that cannot fit on a line of its own.
    WordOrGlyph,
}

/// Shapes `text` and lays it out into a block `width` pixels wide.
///
/// `width` of `None` means unbounded: the block is as wide as its longest line, which is
/// what a measure pass asks for before it knows the available space.
#[must_use]
pub fn shape(
    fonts: &mut FontSystem,
    text: &str,
    style: &TextStyle,
    width: Option<f32>,
    wrapping: Wrapping,
) -> TextLayout {
    // cosmic-text panics with "no default font found" when asked to shape anything at all
    // without a font to shape it with. An engine must not die because a font file failed to
    // load, or because an application shipped without one, so the empty case is answered
    // here: one line of the right height, no glyphs.
    if fonts.is_empty() || text.is_empty() {
        return TextLayout::empty(text, style, width);
    }

    let metrics = Metrics::new(style.font_size.max(1.0), style.line_height.max(1.0));
    let mut buffer = Buffer::new(&mut fonts.inner, metrics);
    buffer.set_wrap(match wrapping {
        Wrapping::None => Wrap::None,
        Wrapping::Word => Wrap::Word,
        Wrapping::WordOrGlyph => Wrap::WordOrGlyph,
    });
    buffer.set_size(width, None);

    let mut attrs = Attrs::new()
        .weight(Weight(style.weight))
        .style(if style.italic {
            cosmic_text::Style::Italic
        } else {
            cosmic_text::Style::Normal
        });
    if let Some(first) = style.families.first() {
        attrs = attrs.family(Family::Name(first));
    }
    // `Shaping::Advanced` is the one that handles the cases M4's acceptance names: CJK,
    // emoji, ligatures and combining marks. `Basic` is faster and wrong for all of them.
    buffer.set_text(text, &attrs, Shaping::Advanced, None);
    buffer.shape_until_scroll(&mut fonts.inner, false);

    collect(fonts, buffer, text, style, width)
}

/// Walks cosmic-text's output into ours.
fn collect(
    fonts: &mut FontSystem,
    buffer: Buffer,
    text: &str,
    style: &TextStyle,
    width: Option<f32>,
) -> TextLayout {
    let mut lines: Vec<Line> = Vec::new();
    let mut runs: Vec<ShapedRun> = Vec::new();
    let mut glyphs: Vec<Glyph> = Vec::new();
    let mut widest = 0.0_f32;

    for layout_run in buffer.layout_runs() {
        let line_index = lines.len();
        let first_run = runs.len();
        let line_top = layout_run.line_top;
        let baseline = layout_run.line_y - layout_run.line_top;

        // Group glyphs into runs of one font, size and direction. cosmic-text hands back a
        // whole visual line at a time, which can mix all three.
        let mut run_start = glyphs.len();
        let mut current: Option<(FontId, f32, Direction)> = None;
        let mut run_range: Option<Range<usize>> = None;

        for glyph in layout_run.glyphs {
            let font = fonts.font_id(glyph.font_id);
            let direction = if glyph.level.is_rtl() {
                Direction::Rtl
            } else {
                Direction::Ltr
            };
            let key = (font, glyph.font_size, direction);

            if current.is_some_and(|open| open != key) {
                push_run(
                    &mut runs,
                    run_start,
                    glyphs.len(),
                    current,
                    &mut run_range,
                    line_index,
                );
                run_start = glyphs.len();
            }
            current = Some(key);
            run_range = Some(match run_range.take() {
                Some(range) if run_start < glyphs.len() => {
                    range.start.min(glyph.start)..range.end.max(glyph.end)
                }
                _ => glyph.start..glyph.end,
            });

            glyphs.push(Glyph {
                id: glyph.glyph_id,
                font,
                cluster: glyph.start..glyph.end,
                position: Point::new(
                    glyph.x + glyph.x_offset,
                    line_top + baseline + glyph.y_offset,
                ),
                advance: glyph.w,
                font_size: glyph.font_size,
            });
        }
        push_run(
            &mut runs,
            run_start,
            glyphs.len(),
            current,
            &mut run_range,
            line_index,
        );

        let line_width = layout_run.line_w;
        widest = widest.max(line_width);
        let byte_range = line_byte_range(&glyphs, first_run, &runs, layout_run.text, text);

        lines.push(Line {
            range: byte_range,
            bounds: Rect::from_xywh(
                0.0,
                line_top,
                width.unwrap_or(line_width),
                layout_run.line_height,
            ),
            baseline,
            width: line_width,
            runs: first_run..runs.len(),
        });
    }

    // Reachable when the text is entirely unshapeable — every character missing from every
    // font. `shape` already short-circuits the empty-string and no-font cases.
    if lines.is_empty() {
        lines.push(Line {
            range: 0..0,
            bounds: Rect::from_xywh(0.0, 0.0, width.unwrap_or(0.0), style.line_height),
            baseline: style.line_height * 0.8,
            width: 0.0,
            runs: 0..0,
        });
    }

    let height = lines
        .last()
        .map_or(style.line_height, |line| line.bounds.max_y());

    TextLayout {
        text: text.to_owned(),
        // The *content* extent, not the width that was offered. A measure pass asks how big
        // the text is, and answering with the container's width makes every text node fill
        // its parent — which puts centred text in the wrong place and makes a paragraph's
        // intrinsic width meaningless.
        size: Size::new(widest, height),
        lines,
        runs,
        glyphs,
        buffer: Some(buffer),
    }
}

fn push_run(
    runs: &mut Vec<ShapedRun>,
    start: usize,
    end: usize,
    key: Option<(FontId, f32, Direction)>,
    range: &mut Option<Range<usize>>,
    line: usize,
) {
    let (Some((font, font_size, direction)), Some(byte_range)) = (key, range.take()) else {
        return;
    };
    if start == end {
        return;
    }
    runs.push(ShapedRun {
        range: byte_range,
        direction,
        font,
        font_size,
        glyphs: start..end,
        line,
    });
}

/// The byte range a visual line covers.
///
/// Derived from the glyphs rather than from cosmic-text's per-line text, because a wrapped
/// line's `text` is a borrowed slice whose offset into the original is not exposed. Trailing
/// whitespace that wrapped away has no glyph, so the range is extended to the start of the
/// next line by the caller of `lines()`; here it is the glyph extent.
fn line_byte_range(
    glyphs: &[Glyph],
    first_run: usize,
    runs: &[ShapedRun],
    line_text: &str,
    _text: &str,
) -> Range<usize> {
    let _ = line_text;
    let Some(run_slice) = runs.get(first_run..) else {
        return 0..0;
    };
    let mut start = usize::MAX;
    let mut end = 0;
    for run in run_slice {
        for glyph in &glyphs[run.glyphs.clone()] {
            start = start.min(glyph.cluster.start);
            end = end.max(glyph.cluster.end);
        }
    }
    if start == usize::MAX {
        0..0
    } else {
        start..end
    }
}
