//! Offscreen rendering and pixel readback.
//!
//! This is how the renderer is tested. A render snapshot test constructs a display list,
//! draws it here, and asserts on pixels — no window, no compositor, no human looking at a
//! screen, and the same code path a window uses for everything except surface acquisition.

use crisol_display_list::Color;

use crate::gpu::Gpu;

/// The colour format offscreen targets use.
///
/// sRGB, to match what a window surface gives us, so a snapshot taken here is directly
/// comparable to what a user sees (DECISIONS D-15).
pub const HEADLESS_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// `copy_texture_to_buffer` requires each row to start on a 256-byte boundary.
const COPY_ALIGNMENT: u32 = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;

/// An offscreen colour target that can be read back to the CPU.
#[derive(Debug)]
pub struct HeadlessTarget {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    readback: wgpu::Buffer,
    width: u32,
    height: u32,
    padded_bytes_per_row: u32,
}

impl HeadlessTarget {
    /// Allocates a `width` x `height` target and the staging buffer to read it back with.
    ///
    /// # Panics
    ///
    /// Panics when either dimension is zero.
    #[must_use]
    pub fn new(gpu: &Gpu, width: u32, height: u32) -> Self {
        assert!(width > 0 && height > 0, "headless target must have area");

        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("crisol headless target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: HEADLESS_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        let unpadded_bytes_per_row = width * 4;
        let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(COPY_ALIGNMENT) * COPY_ALIGNMENT;

        let readback = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("crisol headless readback"),
            size: u64::from(padded_bytes_per_row) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        Self {
            texture,
            view,
            readback,
            width,
            height,
            padded_bytes_per_row,
        }
    }

    /// The colour attachment to render into.
    #[must_use]
    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    /// Width in physical pixels.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Height in physical pixels.
    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Copies the target to the CPU as tightly packed straight-alpha sRGB RGBA bytes.
    ///
    /// Blocks until the GPU is done, which is fine here and never acceptable in a frame
    /// loop.
    ///
    /// # Panics
    ///
    /// Panics when the readback buffer cannot be mapped, which means the device was lost.
    #[must_use]
    pub fn read_pixels(&self, gpu: &Gpu) -> Pixels {
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("crisol readback"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &self.readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(self.padded_bytes_per_row),
                    rows_per_image: Some(self.height),
                },
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
        gpu.queue.submit(Some(encoder.finish()));

        let slice = self.readback.slice(..);
        let (sender, receiver) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            // The receiver is only dropped if this thread panicked, in which case the
            // panic is the interesting failure, not this send.
            let _ = sender.send(result);
        });
        gpu.wait_idle();
        receiver
            .recv()
            .expect("map_async callback was never invoked")
            .expect("readback buffer could not be mapped");

        let mut data = Vec::with_capacity(self.width as usize * self.height as usize * 4);
        {
            let mapped = slice
                .get_mapped_range()
                .expect("mapped range was not available after a successful map_async");
            let row_bytes = self.width as usize * 4;
            for row in 0..self.height as usize {
                let start = row * self.padded_bytes_per_row as usize;
                data.extend_from_slice(&mapped[start..start + row_bytes]);
            }
        }
        self.readback.unmap();

        Pixels {
            data,
            width: self.width,
            height: self.height,
        }
    }
}

/// A readback image: tightly packed straight-alpha sRGB RGBA bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pixels {
    data: Vec<u8>,
    width: u32,
    height: u32,
}

impl Pixels {
    /// Width in pixels.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Height in pixels.
    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Raw bytes, four per pixel, row-major, no padding.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }

    /// The pixel at `(x, y)`.
    ///
    /// # Panics
    ///
    /// Panics when the coordinates are out of bounds — in a test that is a mistake worth
    /// stopping for, not a `None` to be unwrapped.
    #[must_use]
    pub fn at(&self, x: u32, y: u32) -> [u8; 4] {
        assert!(
            x < self.width && y < self.height,
            "({x}, {y}) is outside a {}x{} image",
            self.width,
            self.height
        );
        let i = (y as usize * self.width as usize + x as usize) * 4;
        [
            self.data[i],
            self.data[i + 1],
            self.data[i + 2],
            self.data[i + 3],
        ]
    }

    /// True when the pixel at `(x, y)` is within `tolerance` of `expected` on every
    /// channel.
    ///
    /// Tolerance exists because the shader's sRGB round trip and the driver's blending are
    /// not bit-exact across backends. One or two levels of drift is normal; ten is a bug.
    #[must_use]
    pub fn matches(&self, x: u32, y: u32, expected: Color, tolerance: u8) -> bool {
        let actual = self.at(x, y);
        let expected = expected.to_rgba8();
        actual
            .iter()
            .zip(expected.iter())
            .all(|(a, e)| a.abs_diff(*e) <= tolerance)
    }

    /// Asserts that the pixel at `(x, y)` is `expected` within `tolerance`.
    ///
    /// # Panics
    ///
    /// Panics with both colours in the message when it is not.
    pub fn assert_pixel(&self, x: u32, y: u32, expected: Color, tolerance: u8) {
        assert!(
            self.matches(x, y, expected, tolerance),
            "pixel ({x}, {y}) is {:?}, expected {:?} (tolerance {tolerance})",
            self.at(x, y),
            expected.to_rgba8()
        );
    }
}
