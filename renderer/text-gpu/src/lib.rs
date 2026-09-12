//! glyphon integration: glyph atlas and GPU text.
//!
//! glyphon owns the atlas, the rasterisation cache and the draw. This crate owns the part
//! that makes it fit the engine: drawing text *in painter's order*, interleaved with
//! everything else, inside the single render pass the tile-GPU constraint requires
//! (DECISIONS D-09, D-14).
//!
//! That last point is the whole reason this file exists. A `glyphon::TextRenderer` prepares
//! a set of text areas and then draws all of them with one call, so a single renderer can
//! only put *all* text above or below *all* rectangles. A run of text between two
//! backgrounds therefore needs a renderer of its own, and this crate pools them.

#![doc(html_root_url = "https://docs.rs/crisol-text-gpu/0.0.0")]

use crisol_display_list::{Color, Point, Rect, TextId};
use crisol_text::{FontSystem, TextLayout};

/// Somewhere the renderer can look up shaped text by id.
///
/// A trait rather than a concrete map so the renderer does not have to depend on whatever
/// produced the text — `crisol-layout` returns a `NodeMap`, a test hands over a `Vec`, and
/// an application may have its own store.
pub trait TextSource {
    /// The shaped text registered under `id`.
    fn get(&self, id: TextId) -> Option<&TextLayout>;
}

impl TextSource for () {
    fn get(&self, _id: TextId) -> Option<&TextLayout> {
        None
    }
}

/// One block of text to draw.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextArea {
    /// Which shaped block.
    pub text: TextId,
    /// Where its top-left corner goes, in physical pixels.
    pub origin: Point,
    /// Colour for glyphs with no colour of their own.
    pub color: Color,
    /// Clip bounds in physical pixels. Glyphs outside are not drawn.
    pub clip: Rect,
    /// Logical-to-physical scale the text was *not* shaped at, and must be drawn at.
    pub scale: f32,
}

/// Draws shaped text.
///
/// Holds the atlas and the rasterisation cache across frames, which is what makes the second
/// frame cheap: a glyph is rasterised once and then lives in the atlas.
pub struct GlyphRenderer {
    cache: glyphon::Cache,
    atlas: glyphon::TextAtlas,
    viewport: glyphon::Viewport,
    swash: glyphon::SwashCache,
    /// One per text run in the frame, reused across frames. See the module comment: a
    /// `glyphon::TextRenderer` draws everything it prepared at once, so painter's order
    /// costs one renderer per run rather than one per frame.
    renderers: Vec<glyphon::TextRenderer>,
    prepared: usize,
}

impl std::fmt::Debug for GlyphRenderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GlyphRenderer")
            .field("runs", &self.prepared)
            .field("pooled_renderers", &self.renderers.len())
            .finish_non_exhaustive()
    }
}

impl GlyphRenderer {
    /// Builds the atlas for a colour target of `format`.
    #[must_use]
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let cache = glyphon::Cache::new(device);
        let atlas = glyphon::TextAtlas::new(device, queue, &cache, format);
        let viewport = glyphon::Viewport::new(device, &cache);
        Self {
            cache,
            atlas,
            viewport,
            swash: glyphon::SwashCache::new(),
            renderers: Vec::new(),
            prepared: 0,
        }
    }

    /// Starts a frame targeting a surface of `width` x `height` physical pixels.
    pub fn begin_frame(&mut self, queue: &wgpu::Queue, width: u32, height: u32) {
        self.viewport.update(
            queue,
            glyphon::Resolution {
                width: width.max(1),
                height: height.max(1),
            },
        );
        self.prepared = 0;
    }

    /// Prepares one run of text areas, to be drawn together and in order.
    ///
    /// Returns the run's index, which [`Self::draw`] takes. Returns `None` when nothing in
    /// the run resolved to shaped text — a caller that never uploaded it, most likely.
    ///
    /// # Errors
    ///
    /// Returns [`glyphon::PrepareError`] when the atlas is full and cannot grow, which in
    /// practice means a single frame asked for more distinct glyphs than the GPU will hold.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        fonts: &mut FontSystem,
        source: &impl TextSource,
        areas: &[TextArea],
    ) -> Result<Option<usize>, glyphon::PrepareError> {
        let resolved: Vec<_> = areas
            .iter()
            .filter_map(|area| {
                let layout = source.get(area.text)?;
                let buffer = layout.cosmic_buffer()?;
                Some(glyphon::TextArea {
                    buffer,
                    left: area.origin.x,
                    top: area.origin.y,
                    scale: area.scale,
                    bounds: glyphon::TextBounds {
                        left: area.clip.min_x() as i32,
                        top: area.clip.min_y() as i32,
                        right: area.clip.max_x().ceil() as i32,
                        bottom: area.clip.max_y().ceil() as i32,
                    },
                    default_color: to_glyphon_color(area.color),
                    custom_glyphs: &[],
                })
            })
            .collect();

        if resolved.is_empty() {
            return Ok(None);
        }

        let index = self.prepared;
        if index == self.renderers.len() {
            self.renderers.push(glyphon::TextRenderer::new(
                &mut self.atlas,
                device,
                wgpu::MultisampleState::default(),
                None,
            ));
        }
        self.renderers[index].prepare(
            device,
            queue,
            fonts.cosmic_font_system(),
            &mut self.atlas,
            &self.viewport,
            resolved,
            &mut self.swash,
        )?;
        self.prepared += 1;
        Ok(Some(index))
    }

    /// Draws a prepared run into an open render pass.
    ///
    /// # Errors
    ///
    /// Returns [`glyphon::RenderError`] when the atlas was rebuilt between prepare and
    /// render, which means a prepare for a later run evicted this one's glyphs.
    pub fn draw(
        &self,
        run: usize,
        pass: &mut wgpu::RenderPass<'_>,
    ) -> Result<(), glyphon::RenderError> {
        let Some(renderer) = self.renderers.get(run) else {
            return Ok(());
        };
        renderer.render(&self.atlas, &self.viewport, pass)
    }

    /// Drops atlas entries no longer in use.
    ///
    /// Call between frames. Without it the atlas grows to the union of every glyph ever
    /// drawn, which for a document editor is the whole font.
    pub fn trim(&mut self) {
        self.atlas.trim();
    }

    /// How many runs were prepared this frame.
    #[must_use]
    pub fn prepared_runs(&self) -> usize {
        self.prepared
    }

    /// The glyphon cache, for a caller that wants to share it with another renderer.
    #[must_use]
    pub fn cache(&self) -> &glyphon::Cache {
        &self.cache
    }
}

/// Converts a straight sRGB colour to glyphon's.
///
/// glyphon takes 8-bit sRGB and does its own blending, so unlike the rectangle shader there
/// is no linear conversion to do here — see DECISIONS D-15 for why the two differ.
fn to_glyphon_color(color: Color) -> glyphon::Color {
    let [r, g, b, a] = color.to_rgba8();
    glyphon::Color::rgba(r, g, b, a)
}
