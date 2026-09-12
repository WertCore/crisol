//! The renderer's image store.
//!
//! Display lists refer to images by [`ImageId`]; the pixels live here. Keeping bitmap data
//! out of the display list is what lets the list be thrown away and rebuilt every frame
//! without re-uploading anything.

use std::collections::HashMap;

use crisol_display_list::ImageId;

use crate::gpu::Gpu;

/// One uploaded image and the bind group that binds it.
#[derive(Debug)]
pub(crate) struct ImageEntry {
    pub(crate) bind_group: wgpu::BindGroup,
    #[allow(
        dead_code,
        reason = "kept alive for the bind group; read by M6's eviction"
    )]
    texture: wgpu::Texture,
    pub(crate) size: (u32, u32),
}

/// Images the renderer can draw, keyed by the id paint used.
#[derive(Debug)]
pub(crate) struct ImageStore {
    entries: HashMap<ImageId, ImageEntry>,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

impl ImageStore {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("crisol image"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("crisol image sampler"),
            // Clamp rather than repeat: the antialiasing pad samples just outside the
            // source rectangle, and repeating would wrap the opposite edge into it.
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        Self {
            entries: HashMap::new(),
            layout,
            sampler,
        }
    }

    pub(crate) fn layout(&self) -> &wgpu::BindGroupLayout {
        &self.layout
    }

    pub(crate) fn get(&self, id: ImageId) -> Option<&ImageEntry> {
        self.entries.get(&id)
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// Uploads straight-alpha 8-bit sRGB RGBA pixels, replacing any image already at `id`.
    ///
    /// # Panics
    ///
    /// Panics when `pixels` is not exactly `width * height * 4` bytes. A short buffer is a
    /// caller bug that would otherwise show up as garbage on screen.
    pub(crate) fn upload(
        &mut self,
        gpu: &Gpu,
        id: ImageId,
        width: u32,
        height: u32,
        pixels: &[u8],
    ) {
        let expected = width as usize * height as usize * 4;
        assert_eq!(
            pixels.len(),
            expected,
            "image {id:?} is {width}x{height}, which needs {expected} bytes, got {}",
            pixels.len()
        );

        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("crisol image"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // sRGB format: the hardware linearises on sample, so the shader does not have
            // to, and filtering happens in linear light where it belongs.
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("crisol image"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });

        self.entries.insert(
            id,
            ImageEntry {
                bind_group,
                texture,
                size: (width, height),
            },
        );
    }

    /// Drops an image. Returns whether anything was there.
    pub(crate) fn remove(&mut self, id: ImageId) -> bool {
        self.entries.remove(&id).is_some()
    }
}
