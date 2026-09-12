//! Turns a [`DisplayList`] into draw calls.
//!
//! The renderer knows nothing about nodes, style or layout — only about draw commands
//! (DECISIONS D-13). Rectangles all go through one instanced pipeline so a frame is a
//! handful of draw calls rather than one per node (D-14), which is the difference between
//! acceptable and catastrophic on a tile-based mobile GPU.

use bytemuck::{Pod, Zeroable};
use crisol_display_list::{
    Color, Corners, DisplayList, DrawCommand, Edges, ImageCommand, ImageId, Rect, RectCommand,
};

use crate::gpu::Gpu;
use crate::texture::ImageStore;

/// Per-instance data. Both fragment stages read the same struct; `fs_rect` ignores `uv` and
/// `fs_image` reads `fill` as a tint and ignores the border fields.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
struct Instance {
    rect: [f32; 4],
    radii: [f32; 4],
    fill: [f32; 4],
    border_color: [f32; 4],
    border_width: [f32; 4],
    uv: [f32; 4],
}

/// Uniform block. `viewport.xy` is the physical pixel size of the target; `zw` is padding
/// to the 16-byte alignment a uniform binding requires.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
struct Globals {
    viewport: [f32; 4],
}

/// A scissor rectangle in physical pixels, already clamped to the target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Scissor {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

/// A run of instances that can be drawn with one call.
#[derive(Clone, Copy, Debug)]
enum Batch {
    /// `count` consecutive instances starting at `first`, all rectangles.
    Rects {
        first: u32,
        count: u32,
        scissor: Option<Scissor>,
    },
    /// A single textured quad. Images break a rectangle run because they need a different
    /// pipeline and bind group.
    Image {
        instance: u32,
        image: ImageId,
        scissor: Option<Scissor>,
    },
}

/// Where a frame is drawn.
#[derive(Clone, Copy, Debug)]
pub struct FrameTarget<'a> {
    /// The colour attachment.
    pub view: &'a wgpu::TextureView,
    /// Target width in physical pixels.
    pub width: u32,
    /// Target height in physical pixels.
    pub height: u32,
    /// Logical-to-physical pixel ratio. Display lists are authored in logical pixels; this
    /// is the only place the conversion happens.
    pub scale_factor: f32,
}

/// Counters for one frame, for the instrumentation M6's acceptance test needs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameStats {
    /// Commands in the display list.
    pub commands: usize,
    /// Instances written to the instance buffer.
    pub instances: usize,
    /// Draw calls issued.
    pub draw_calls: usize,
    /// Image commands skipped because the id was never uploaded.
    pub missing_images: usize,
}

/// Rasterises display lists.
pub struct Renderer {
    gpu: Gpu,
    format: wgpu::TextureFormat,
    rect_pipeline: wgpu::RenderPipeline,
    image_pipeline: wgpu::RenderPipeline,
    globals_buffer: wgpu::Buffer,
    globals_bind_group: wgpu::BindGroup,
    instance_buffer: wgpu::Buffer,
    instance_capacity: usize,
    images: ImageStore,

    // Per-frame scratch, kept across frames so a steady-state frame allocates nothing.
    instances: Vec<Instance>,
    batches: Vec<Batch>,
    clip_stack: Vec<Rect>,
}

impl std::fmt::Debug for Renderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Renderer")
            .field("format", &self.format)
            .field("instance_capacity", &self.instance_capacity)
            .field("images", &self.images.len())
            .finish_non_exhaustive()
    }
}

/// Instances the buffer is sized for on creation. A frame that needs more grows it; a frame
/// that needs fewer keeps the allocation.
const INITIAL_INSTANCE_CAPACITY: usize = 1024;

const INSTANCE_ATTRIBUTES: [wgpu::VertexAttribute; 6] = wgpu::vertex_attr_array![
    0 => Float32x4,
    1 => Float32x4,
    2 => Float32x4,
    3 => Float32x4,
    4 => Float32x4,
    5 => Float32x4,
];

impl Renderer {
    /// Builds the pipelines for a colour target of `format`.
    ///
    /// `format` should be an sRGB format. See DECISIONS D-15: the shader writes linear
    /// premultiplied colour and relies on the hardware to encode.
    #[must_use]
    pub fn new(gpu: &Gpu, format: wgpu::TextureFormat) -> Self {
        let device = &gpu.device;

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("crisol draw"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/draw.wgsl").into()),
        });

        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("crisol globals"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let globals_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("crisol globals"),
            size: size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let globals_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("crisol globals"),
            layout: &globals_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals_buffer.as_entire_binding(),
            }],
        });

        let images = ImageStore::new(device);

        let instance_layout = wgpu::VertexBufferLayout {
            array_stride: size_of::<Instance>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &INSTANCE_ATTRIBUTES,
        };

        // Premultiplied source-over. The shader already multiplied colour by alpha, so the
        // source factor is One rather than SrcAlpha.
        let blend = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
        };

        let make_pipeline =
            |label: &str, entry: &str, bind_group_layouts: &[Option<&wgpu::BindGroupLayout>]| {
                let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some(label),
                    bind_group_layouts,
                    immediate_size: 0,
                });
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some(label),
                    layout: Some(&layout),
                    vertex: wgpu::VertexState {
                        module: &shader,
                        entry_point: Some("vs_main"),
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                        buffers: &[Some(instance_layout.clone())],
                    },
                    primitive: wgpu::PrimitiveState {
                        topology: wgpu::PrimitiveTopology::TriangleStrip,
                        strip_index_format: None,
                        front_face: wgpu::FrontFace::Ccw,
                        // No culling: the quad is generated in a fixed winding and there is no
                        // 3D to cull against.
                        cull_mode: None,
                        unclipped_depth: false,
                        polygon_mode: wgpu::PolygonMode::Fill,
                        conservative: false,
                    },
                    depth_stencil: None,
                    multiview_mask: None,
                    multisample: wgpu::MultisampleState::default(),
                    fragment: Some(wgpu::FragmentState {
                        module: &shader,
                        entry_point: Some(entry),
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                        targets: &[Some(wgpu::ColorTargetState {
                            format,
                            blend: Some(blend),
                            write_mask: wgpu::ColorWrites::ALL,
                        })],
                    }),
                    cache: None,
                })
            };

        let rect_pipeline = make_pipeline("crisol rect", "fs_rect", &[Some(&globals_layout)]);
        let image_pipeline = make_pipeline(
            "crisol image",
            "fs_image",
            &[Some(&globals_layout), Some(images.layout())],
        );

        let instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("crisol instances"),
            size: (INITIAL_INSTANCE_CAPACITY * size_of::<Instance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            gpu: gpu.clone(),
            format,
            rect_pipeline,
            image_pipeline,
            globals_buffer,
            globals_bind_group,
            instance_buffer,
            instance_capacity: INITIAL_INSTANCE_CAPACITY,
            images,
            instances: Vec::with_capacity(INITIAL_INSTANCE_CAPACITY),
            batches: Vec::new(),
            clip_stack: Vec::new(),
        }
    }

    /// The colour format this renderer's pipelines were built for.
    #[must_use]
    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }

    /// The device this renderer draws with.
    #[must_use]
    pub fn gpu(&self) -> &Gpu {
        &self.gpu
    }

    /// Uploads straight-alpha 8-bit sRGB RGBA pixels under `id`, replacing any previous
    /// image with that id.
    ///
    /// # Panics
    ///
    /// Panics when `pixels` is not exactly `width * height * 4` bytes.
    pub fn upload_image(&mut self, id: ImageId, width: u32, height: u32, pixels: &[u8]) {
        self.images.upload(&self.gpu, id, width, height, pixels);
    }

    /// Drops an image. Returns whether anything was there.
    pub fn remove_image(&mut self, id: ImageId) -> bool {
        self.images.remove(id)
    }

    /// The pixel size of an uploaded image.
    #[must_use]
    pub fn image_size(&self, id: ImageId) -> Option<(u32, u32)> {
        self.images.get(id).map(|entry| entry.size)
    }

    /// Draws `list` into `target`, clearing to the list's background colour first.
    ///
    /// Returns per-frame counters. Nothing is presented — the caller owns the swapchain.
    pub fn render(&mut self, target: FrameTarget<'_>, list: &DisplayList) -> FrameStats {
        let mut stats = FrameStats {
            commands: list.len(),
            ..FrameStats::default()
        };

        self.build_batches(list, &target, &mut stats);
        self.upload_frame_data(&target);

        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("crisol frame"),
            });

        {
            // One render pass for the whole frame. Beginning a second pass would force a
            // tile-based GPU to flush and reload the tile, which DECISIONS D-09 rules out.
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("crisol frame"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(clear_color(list.background)),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                multiview_mask: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            pass.set_bind_group(0, &self.globals_bind_group, &[]);
            pass.set_vertex_buffer(0, self.instance_buffer.slice(..));

            let mut current_scissor: Option<Scissor> = None;
            let mut current_pipeline: Option<bool> = None;

            for batch in &self.batches {
                let (scissor, is_rect) = match batch {
                    Batch::Rects { scissor, .. } => (*scissor, true),
                    Batch::Image { scissor, .. } => (*scissor, false),
                };

                if current_scissor != scissor {
                    match scissor {
                        Some(s) => pass.set_scissor_rect(s.x, s.y, s.width, s.height),
                        None => pass.set_scissor_rect(0, 0, target.width, target.height),
                    }
                    current_scissor = scissor;
                }

                if current_pipeline != Some(is_rect) {
                    pass.set_pipeline(if is_rect {
                        &self.rect_pipeline
                    } else {
                        &self.image_pipeline
                    });
                    current_pipeline = Some(is_rect);
                }

                match batch {
                    Batch::Rects { first, count, .. } => {
                        pass.draw(0..4, *first..(*first + *count));
                        stats.draw_calls += 1;
                    }
                    Batch::Image {
                        instance, image, ..
                    } => {
                        let Some(entry) = self.images.get(*image) else {
                            continue;
                        };
                        pass.set_bind_group(1, &entry.bind_group, &[]);
                        pass.draw(0..4, *instance..(*instance + 1));
                        stats.draw_calls += 1;
                    }
                }
            }
        }

        self.gpu.queue.submit(Some(encoder.finish()));
        stats
    }

    /// Walks the command list once, producing instances and the runs that draw them.
    fn build_batches(
        &mut self,
        list: &DisplayList,
        target: &FrameTarget<'_>,
        stats: &mut FrameStats,
    ) {
        self.instances.clear();
        self.batches.clear();
        self.clip_stack.clear();

        let scale = target.scale_factor;

        for command in list.commands() {
            match command {
                DrawCommand::PushClip(rect) => {
                    // The builder already intersected this with the enclosing clip, but the
                    // renderer must not depend on a producer being well behaved.
                    let effective = match self.clip_stack.last() {
                        Some(current) => current.intersection(*rect).unwrap_or(Rect::ZERO),
                        None => *rect,
                    };
                    self.clip_stack.push(effective);
                }
                DrawCommand::PopClip => {
                    self.clip_stack.pop();
                }
                DrawCommand::Rect(rect) => {
                    let Some(scissor) = self.current_scissor(target, scale) else {
                        continue;
                    };
                    let first = self.instances.len() as u32;
                    self.instances.push(Instance::from_rect(rect, scale));
                    match self.batches.last_mut() {
                        Some(Batch::Rects {
                            count,
                            scissor: existing,
                            ..
                        }) if *existing == scissor => *count += 1,
                        _ => self.batches.push(Batch::Rects {
                            first,
                            count: 1,
                            scissor,
                        }),
                    }
                }
                DrawCommand::Image(image) => {
                    if self.images.get(image.image).is_none() {
                        stats.missing_images += 1;
                        continue;
                    }
                    let Some(scissor) = self.current_scissor(target, scale) else {
                        continue;
                    };
                    let instance = self.instances.len() as u32;
                    self.instances.push(Instance::from_image(image, scale));
                    self.batches.push(Batch::Image {
                        instance,
                        image: image.image,
                        scissor,
                    });
                }
            }
        }

        stats.instances = self.instances.len();
    }

    /// The scissor for the clip currently in force.
    ///
    /// Returns `Some(None)` for "no clip", `Some(Some(rect))` for a clip, and `None` when
    /// the clip is empty and the caller should skip the command entirely.
    #[allow(
        clippy::option_option,
        reason = "the three states are genuinely distinct"
    )]
    fn current_scissor(&self, target: &FrameTarget<'_>, scale: f32) -> Option<Option<Scissor>> {
        let Some(clip) = self.clip_stack.last() else {
            return Some(None);
        };
        Scissor::from_logical(*clip, scale, target.width, target.height).map(Some)
    }

    /// Writes this frame's uniforms and instances, growing the instance buffer if needed.
    fn upload_frame_data(&mut self, target: &FrameTarget<'_>) {
        let globals = Globals {
            viewport: [target.width as f32, target.height as f32, 0.0, 0.0],
        };
        self.gpu
            .queue
            .write_buffer(&self.globals_buffer, 0, bytemuck::bytes_of(&globals));

        if self.instances.is_empty() {
            return;
        }

        if self.instances.len() > self.instance_capacity {
            // Grow geometrically so a list that keeps getting slightly bigger does not
            // reallocate every frame.
            let capacity = self.instances.len().next_power_of_two();
            self.instance_buffer = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("crisol instances"),
                size: (capacity * size_of::<Instance>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.instance_capacity = capacity;
        }

        self.gpu.queue.write_buffer(
            &self.instance_buffer,
            0,
            bytemuck::cast_slice(&self.instances),
        );
    }
}

impl Instance {
    fn from_rect(command: &RectCommand, scale: f32) -> Self {
        let rect = command.rect.scale(scale);
        Self {
            rect: [
                rect.origin.x,
                rect.origin.y,
                rect.size.width,
                rect.size.height,
            ],
            radii: corners_to_array(command.radii.clamped_to(command.rect.size).scale(scale)),
            fill: command.fill.to_array(),
            border_color: command.border_color.to_array(),
            border_width: edges_to_array(command.border_width.scale(scale)),
            uv: [0.0, 0.0, 0.0, 0.0],
        }
    }

    fn from_image(command: &ImageCommand, scale: f32) -> Self {
        let rect = command.rect.scale(scale);
        Self {
            rect: [
                rect.origin.x,
                rect.origin.y,
                rect.size.width,
                rect.size.height,
            ],
            radii: corners_to_array(command.radii.clamped_to(command.rect.size).scale(scale)),
            fill: command.tint.to_array(),
            border_color: [0.0; 4],
            border_width: [0.0; 4],
            uv: [
                command.uv.origin.x,
                command.uv.origin.y,
                command.uv.size.width,
                command.uv.size.height,
            ],
        }
    }
}

/// CSS order: top-left, top-right, bottom-right, bottom-left. The shader relies on it.
fn corners_to_array(corners: Corners) -> [f32; 4] {
    [
        corners.top_left,
        corners.top_right,
        corners.bottom_right,
        corners.bottom_left,
    ]
}

/// CSS order: top, right, bottom, left. The shader relies on it.
fn edges_to_array(edges: Edges) -> [f32; 4] {
    [
        edges.top.max(0.0),
        edges.right.max(0.0),
        edges.bottom.max(0.0),
        edges.left.max(0.0),
    ]
}

/// The clear colour, converted the same way the shader converts fills.
///
/// `wgpu::Color` on an sRGB target is interpreted as linear, so the sRGB-encoded components
/// a display list carries have to be decoded here too — otherwise the background is visibly
/// lighter than the same colour drawn as a rectangle.
fn clear_color(color: Color) -> wgpu::Color {
    fn to_linear(c: f32) -> f64 {
        let c = f64::from(c.clamp(0.0, 1.0));
        if c <= 0.040_45 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }
    wgpu::Color {
        r: to_linear(color.r) * f64::from(color.a),
        g: to_linear(color.g) * f64::from(color.a),
        b: to_linear(color.b) * f64::from(color.a),
        a: f64::from(color.a),
    }
}

impl Scissor {
    /// Converts a logical-pixel clip to an integer physical-pixel scissor clamped to the
    /// target, or `None` when nothing survives.
    ///
    /// The bounds are rounded outwards: a pixel the clip only partially covers is kept, and
    /// the shader's own coverage decides what it looks like. Rounding inwards would eat a
    /// row of pixels off every clipped edge.
    fn from_logical(clip: Rect, scale: f32, width: u32, height: u32) -> Option<Self> {
        let physical = clip.scale(scale);
        let x0 = physical.min_x().floor().max(0.0) as u32;
        let y0 = physical.min_y().floor().max(0.0) as u32;
        let x1 = (physical.max_x().ceil().max(0.0) as u32).min(width);
        let y1 = (physical.max_y().ceil().max(0.0) as u32).min(height);
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        Some(Self {
            x: x0.min(width),
            y: y0.min(height),
            width: x1 - x0,
            height: y1 - y0,
        })
    }
}
